use rustc_abi::{Float, HasDataLayout, Niche, Primitive, TagEncoding, Variants};
use rustc_middle::ty::layout::{LayoutCx, TyAndLayout};
use rustc_middle::ty::{self, AdtDef, Ty, TyCtxt};

use nichy::{
    DiscriminantInfo, EnumLayout, EnumStrategy, FieldLayout, NicheInfo, StructLayout, TypeLayout,
    TypeLayoutKind, VariantLayout,
};

pub fn convert_layout<'tcx>(
    tcx: TyCtxt<'tcx>,
    typing_env: ty::TypingEnv<'tcx>,
    name: &str,
    ty: Ty<'tcx>,
    layout: TyAndLayout<'tcx>,
) -> TypeLayout {
    let cx = LayoutCx::new(tcx, typing_env);
    let largest_niche = layout.largest_niche.map(|n| convert_niche(&cx, &n, None));

    match ty.kind() {
        ty::Adt(adt_def, _) if adt_def.is_enum() => {
            convert_enum(tcx, &cx, name, *adt_def, layout, largest_niche)
        }
        ty::Adt(adt_def, _) if adt_def.is_struct() => {
            convert_struct(tcx, &cx, name, *adt_def, layout, largest_niche)
        }
        _ => TypeLayout {
            name: name.into(),
            size: layout.size.bytes(),
            alignment: layout.align.abi.bytes(),
            hover_info: None,
            largest_niche,
            kind: TypeLayoutKind::Opaque,
        },
    }
}

fn convert_struct<'tcx>(
    _tcx: TyCtxt<'tcx>,
    cx: &LayoutCx<'tcx>,
    name: &str,
    adt_def: AdtDef<'tcx>,
    layout: TyAndLayout<'tcx>,
    largest_niche: Option<NicheInfo>,
) -> TypeLayout {
    let fields = variant_fields(cx, &layout, adt_def.non_enum_variant());
    let used: u64 = fields.iter().map(|f| f.size).sum();
    let padding_bytes = layout.size.bytes().saturating_sub(used);

    TypeLayout {
        name: name.into(),
        size: layout.size.bytes(),
        alignment: layout.align.abi.bytes(),
        hover_info: None,
        largest_niche,
        kind: TypeLayoutKind::Struct(StructLayout {
            fields,
            padding_bytes,
        }),
    }
}

fn convert_enum<'tcx>(
    tcx: TyCtxt<'tcx>,
    cx: &LayoutCx<'tcx>,
    name: &str,
    adt_def: AdtDef<'tcx>,
    layout: TyAndLayout<'tcx>,
    largest_niche: Option<NicheInfo>,
) -> TypeLayout {
    let size = layout.size.bytes();
    let alignment = layout.align.abi.bytes();
    let remaining = largest_niche.as_ref().map(|n| n.available);

    let enum_layout = match layout.variants {
        Variants::Empty => EnumLayout {
            strategy: EnumStrategy::Single,
            remaining_niches: remaining,
            variants: vec![],
            niche: largest_niche.clone(),
            discriminant: None,
        },

        Variants::Single { index } => {
            let variant_def = adt_def.variant(index);
            let fields = variant_fields(cx, &layout, variant_def);
            EnumLayout {
                strategy: EnumStrategy::Single,
                remaining_niches: remaining,
                variants: vec![VariantLayout {
                    name: variant_def.name.to_string(),
                    size,
                    alignment,
                    fields,
                    discr_value: None,
                }],
                niche: largest_niche.clone(),
                discriminant: None,
            }
        }

        Variants::Multiple {
            tag,
            ref tag_encoding,
            tag_field,
            ref variants,
        } => {
            let tag_offset = layout.fields.offset(tag_field.into()).bytes();
            let tag_size_bytes = tag.size(cx).bytes();

            let (strategy, discriminant) = match tag_encoding {
                TagEncoding::Direct => (
                    EnumStrategy::Tagged {
                        discriminant_size: tag_size_bytes,
                    },
                    Some(DiscriminantInfo::Direct {
                        tag_field_index: tag_field.into(),
                        tag_offset,
                        tag_size: tag_size_bytes,
                    }),
                ),
                TagEncoding::Niche {
                    untagged_variant,
                    niche_variants,
                    niche_start,
                } => {
                    let untagged_name = adt_def.variant(*untagged_variant).name.to_string();

                    let max_payload = variants.iter().map(|v| v.size.bytes()).max().unwrap_or(0);
                    let hypothetical_tagged = max_payload + tag_size_bytes;
                    let savings = hypothetical_tagged.saturating_sub(size);

                    (
                        EnumStrategy::NicheOptimized {
                            savings,
                            tagged_size: hypothetical_tagged,
                        },
                        Some(DiscriminantInfo::Niche {
                            tag_field_index: tag_field.into(),
                            tag_offset,
                            untagged_variant: untagged_name,
                            niche_start: *niche_start,
                            niche_variants_start: niche_variants.start().as_usize(),
                            niche_variants_end: niche_variants.end().as_usize(),
                        }),
                    )
                }
            };

            let niche_info = if let TagEncoding::Niche {
                untagged_variant, ..
            } = tag_encoding
            {
                let field_name = resolve_niche_field_name(
                    cx,
                    &layout,
                    adt_def,
                    tag_field.into(),
                    *untagged_variant,
                );
                Some(niche_info_from_tag(cx, tag, tag_offset, field_name))
            } else {
                None
            };

            let variant_layouts: Vec<VariantLayout> = adt_def
                .variants()
                .iter_enumerated()
                .map(|(vi, vdef)| {
                    let v_layout = layout.for_variant(cx, vi);
                    let fields = variant_fields(cx, &v_layout, vdef);
                    let discr_value = if matches!(tag_encoding, TagEncoding::Direct) {
                        Some(adt_def.discriminant_for_variant(tcx, vi).val)
                    } else {
                        None
                    };
                    VariantLayout {
                        name: vdef.name.to_string(),
                        size: v_layout.size.bytes(),
                        alignment: v_layout.align.abi.bytes(),
                        fields,
                        discr_value,
                    }
                })
                .collect();

            let remaining = niche_info.as_ref().map(|n| n.available).or(remaining);

            EnumLayout {
                strategy,
                remaining_niches: remaining,
                variants: variant_layouts,
                niche: niche_info,
                discriminant,
            }
        }
    };

    TypeLayout {
        name: name.into(),
        size,
        alignment,
        hover_info: None,
        largest_niche,
        kind: TypeLayoutKind::Enum(enum_layout),
    }
}

fn variant_fields<'tcx>(
    cx: &LayoutCx<'tcx>,
    layout: &TyAndLayout<'tcx>,
    variant_def: &ty::VariantDef,
) -> Vec<FieldLayout> {
    let mut fields: Vec<FieldLayout> = variant_def
        .fields
        .iter()
        .enumerate()
        .map(|(i, fdef)| {
            let f_layout = layout.field(cx, i);
            let largest_niche = f_layout
                .largest_niche
                .map(|n| convert_niche(cx, &n, Some(fdef.name.to_string())));
            FieldLayout {
                name: fdef.name.to_string(),
                typename: format!("{}", f_layout.ty),
                offset: layout.fields.offset(i).bytes(),
                size: f_layout.size.bytes(),
                alignment: f_layout.align.abi.bytes(),
                children: vec![],
                largest_niche,
            }
        })
        .collect();
    fields.sort_by_key(|f| f.offset);
    fields
}

fn resolve_niche_field_name<'tcx>(
    cx: &LayoutCx<'tcx>,
    layout: &TyAndLayout<'tcx>,
    adt_def: AdtDef<'tcx>,
    tag_field_idx: usize,
    untagged_variant: rustc_abi::VariantIdx,
) -> Option<String> {
    let tag_offset = layout.fields.offset(tag_field_idx).bytes();
    let variant_def = adt_def.variant(untagged_variant);
    let v_layout = layout.for_variant(cx, untagged_variant);
    for (fi, fdef) in variant_def.fields.iter().enumerate() {
        let f_offset = v_layout.fields.offset(fi).bytes();
        if f_offset == tag_offset {
            return Some(fdef.name.to_string());
        }
    }
    None
}

fn primitive_type_name(prim: Primitive) -> String {
    match prim {
        Primitive::Int(int, signed) => if signed {
            int.int_ty_str()
        } else {
            int.uint_ty_str()
        }
        .to_string(),
        Primitive::Float(Float::F16) => "f16".to_string(),
        Primitive::Float(Float::F32) => "f32".to_string(),
        Primitive::Float(Float::F64) => "f64".to_string(),
        Primitive::Float(Float::F128) => "f128".to_string(),
        Primitive::Pointer(_) => "ptr".to_string(),
    }
}

fn build_niche_info(
    dl: &impl HasDataLayout,
    offset: u64,
    prim: Primitive,
    valid_range: rustc_abi::WrappingRange,
    field_name: Option<String>,
) -> NicheInfo {
    let niche = Niche {
        offset: rustc_abi::Size::from_bytes(offset),
        value: prim,
        valid_range,
    };
    NicheInfo {
        offset,
        field_name,
        value_type: primitive_type_name(prim),
        value_size: prim.size(dl).bytes(),
        valid_range_start: valid_range.start,
        valid_range_end: valid_range.end,
        available: niche.available(dl),
    }
}

fn niche_info_from_tag(
    dl: &impl HasDataLayout,
    tag: rustc_abi::Scalar,
    tag_offset: u64,
    field_name: Option<String>,
) -> NicheInfo {
    let (prim, valid_range) = match tag {
        rustc_abi::Scalar::Initialized { value, valid_range } => (value, valid_range),
        rustc_abi::Scalar::Union { value } => {
            (value, rustc_abi::WrappingRange::full(value.size(dl)))
        }
    };
    build_niche_info(dl, tag_offset, prim, valid_range, field_name)
}

fn convert_niche(dl: &impl HasDataLayout, niche: &Niche, field_name: Option<String>) -> NicheInfo {
    build_niche_info(
        dl,
        niche.offset.bytes(),
        niche.value,
        niche.valid_range,
        field_name,
    )
}
