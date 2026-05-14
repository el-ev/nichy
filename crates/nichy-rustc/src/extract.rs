use rustc_hir::ItemKind;
use rustc_middle::ty::{self, TyCtxt};

use nichy::TypeLayout;

use crate::convert;

pub fn extract_all_layouts(tcx: TyCtxt<'_>) -> Vec<TypeLayout> {
    let typing_env = ty::TypingEnv::fully_monomorphized();
    let mut results = Vec::new();

    for item_id in tcx.hir_free_items() {
        let item = tcx.hir_item(item_id);

        let is_alias = match item.kind {
            ItemKind::Struct(..) | ItemKind::Enum(..) | ItemKind::Union(..) => false,
            ItemKind::TyAlias(..) => true,
            _ => continue,
        };

        let def_id = item.owner_id.def_id.to_def_id();

        let generics = tcx.generics_of(def_id);
        let counts = generics.own_counts();
        if counts.types > 0 || counts.consts > 0 {
            continue;
        }

        let ty = tcx.type_of(def_id).instantiate_identity().skip_norm_wip();

        let layout = match tcx.layout_of(typing_env.as_query_input(ty)) {
            Ok(layout) => layout,
            Err(_) => continue,
        };

        let name = match &item.kind {
            ItemKind::Struct(ident, ..)
            | ItemKind::Enum(ident, ..)
            | ItemKind::Union(ident, ..)
            | ItemKind::TyAlias(ident, ..) => ident.name.to_string(),
            _ => unreachable!(),
        };

        if is_alias {
            results.push(convert::convert_layout(tcx, typing_env, &name, ty, layout));
            continue;
        }

        if name == "_Probe"
            && let Some(inner) = unwrap_probe(tcx, typing_env, ty)
        {
            results.push(inner);
            continue;
        }

        if name.starts_with("__") {
            continue;
        }

        results.push(convert::convert_layout(tcx, typing_env, &name, ty, layout));
    }

    results
}

fn unwrap_probe<'tcx>(
    tcx: TyCtxt<'tcx>,
    typing_env: ty::TypingEnv<'tcx>,
    probe_ty: ty::Ty<'tcx>,
) -> Option<TypeLayout> {
    let ty::TyKind::Adt(adt_def, args) = probe_ty.kind() else {
        return None;
    };
    if adt_def.non_enum_variant().fields.len() != 1 {
        return None;
    }
    let field_def = &adt_def.non_enum_variant().fields[rustc_abi::FieldIdx::from_u32(0)];
    let field_ty = field_def.ty(tcx, args).skip_norm_wip();
    let field_layout = tcx.layout_of(typing_env.as_query_input(field_ty)).ok()?;
    let name = format!("{field_ty}");
    Some(convert::convert_layout(
        tcx,
        typing_env,
        &name,
        field_ty,
        field_layout,
    ))
}
