use std::fmt::Write;

use nichy::{
    DiscriminantInfo, EnumLayout, EnumStrategy, FieldLayout, StructLayout, TypeLayout,
    TypeLayoutKind, VariantLayout,
};

pub struct Palette {
    pub reset: &'static str,
    pub bold: &'static str,
    pub dim: &'static str,
    pub cyan: &'static str,
    pub magenta: &'static str,
    pub green: &'static str,
    pub underline: &'static str,
    pub field_colors: &'static [&'static str],
}

const ANSI: Palette = Palette {
    reset: "\x1b[0m",
    bold: "\x1b[1m",
    dim: "\x1b[2m",
    cyan: "\x1b[36m",
    magenta: "\x1b[35m",
    green: "\x1b[32m",
    underline: "═",
    field_colors: &[
        "\x1b[38;5;75m",
        "\x1b[38;5;114m",
        "\x1b[38;5;216m",
        "\x1b[38;5;183m",
        "\x1b[38;5;222m",
        "\x1b[38;5;117m",
        "\x1b[38;5;151m",
        "\x1b[38;5;210m",
    ],
};

const PLAIN: Palette = Palette {
    reset: "",
    bold: "",
    dim: "",
    cyan: "",
    magenta: "",
    green: "",
    underline: "=",
    field_colors: &["", "", "", "", "", "", "", ""],
};

pub struct Ctx {
    color: bool,
    #[allow(dead_code)]
    verbose: bool,
    palette: &'static Palette,
}

impl Ctx {
    pub fn new(color: bool, verbose: bool) -> Self {
        Self {
            color,
            verbose,
            palette: if color { &ANSI } else { &PLAIN },
        }
    }

    fn fc(&self, i: usize) -> &'static str {
        let fcs = self.palette.field_colors;
        fcs[i % fcs.len()]
    }

    fn header(&self, out: &mut String, name: &str, size: u64, align: u64) {
        let p = self.palette;
        let _ = write!(
            out,
            "\n{}{}{name}{}\n{}{}{}\n",
            p.bold,
            p.cyan,
            p.reset,
            p.dim,
            p.underline.repeat(60),
            p.reset
        );
        let _ = writeln!(
            out,
            "  size: {}{size}{} bytes, align: {align} bytes",
            p.bold, p.reset
        );
    }
}

pub fn render_type_layout(tl: &TypeLayout, ctx: &Ctx) -> String {
    let p = ctx.palette;
    let mut out = String::new();
    ctx.header(&mut out, &tl.name, tl.size, tl.alignment);
    match &tl.kind {
        TypeLayoutKind::Struct(sl) => render_struct(&mut out, tl, sl, ctx),
        TypeLayoutKind::Enum(el) => render_enum(&mut out, el, ctx),
        TypeLayoutKind::Opaque => {
            if tl.size > 0 {
                let _ = write!(
                    out,
                    "\n  {}scalar / opaque ({} bytes){}\n",
                    p.dim, tl.size, p.reset
                );
            } else {
                let _ = write!(out, "\n  {}zero-sized type{}\n", p.dim, p.reset);
            }
        }
    }
    if let Some(ref niche) = tl.largest_niche
        && !matches!(&tl.kind, TypeLayoutKind::Enum(_))
    {
        let field = niche.field_name.as_deref().unwrap_or("?");
        let _ = write!(
            out,
            "\n  {}niche: `{}` ({}) at +0x{:04x}, {} available{}\n",
            p.dim, field, niche.value_type, niche.offset, niche.available, p.reset,
        );
    }
    if let Some(info) = &tl.hover_info {
        let _ = write!(out, "\n  {}{info}{}\n", p.dim, p.reset);
    }
    out.push('\n');
    out
}

pub fn render_footer(rustc_version: &str, rustc_hash: &str, ctx: &Ctx) -> String {
    let p = ctx.palette;
    let nichy_version = env!("CARGO_PKG_VERSION");
    let rustc_part = if rustc_hash.is_empty() {
        rustc_version.to_string()
    } else {
        format!("{rustc_version} ({rustc_hash})")
    };
    format!("{}nichy {nichy_version} · {rustc_part}{}\n", p.dim, p.reset)
}

fn render_struct(out: &mut String, tl: &TypeLayout, sl: &StructLayout, ctx: &Ctx) {
    render_field_table(out, &sl.fields, tl.size, ctx);
    if tl.size <= 64 {
        render_byte_map(out, &sl.fields, tl.size, ctx);
    }
}

fn render_enum(out: &mut String, el: &EnumLayout, ctx: &Ctx) {
    let p = ctx.palette;
    match &el.strategy {
        EnumStrategy::Single => {}
        EnumStrategy::NicheOptimized { .. } => {
            let _ = write!(out, "\n  {}{}niche optimized{}\n", p.green, p.bold, p.reset);
            if let Some(n) = el.remaining_niches {
                let _ = writeln!(
                    out,
                    "  {}remaining niches: {}{}",
                    p.dim,
                    format_count(n),
                    p.reset
                );
            }
        }
        EnumStrategy::Tagged { discriminant_size } => {
            let _ = write!(
                out,
                "\n  {}tagged enum{} ({}discriminant: {} byte{}{})\n",
                p.magenta,
                p.reset,
                p.dim,
                discriminant_size,
                if *discriminant_size == 1 { "" } else { "s" },
                p.reset
            );
        }
    }
    render_discriminant_info(out, el, ctx);
    render_niche_detail(out, el, ctx);
    for (i, v) in el.variants.iter().enumerate() {
        render_variant(out, v, i, el, ctx);
    }
}

fn variant_tag_suffix(idx: usize, v: &VariantLayout, el: &EnumLayout) -> Option<String> {
    match &el.discriminant {
        Some(DiscriminantInfo::Direct { .. }) => v.discr_value.map(|d| format!("tag = 0x{d:x}")),
        Some(DiscriminantInfo::Niche {
            untagged_variant,
            niche_start,
            niche_variants_start,
            niche_variants_end,
            ..
        }) => {
            if &v.name == untagged_variant {
                Some("untagged".into())
            } else if idx >= *niche_variants_start && idx <= *niche_variants_end {
                let nv = niche_start.wrapping_add((idx - niche_variants_start) as u128);
                Some(format!("niche = 0x{nv:x}"))
            } else {
                None
            }
        }
        None => None,
    }
}

fn render_discriminant_info(out: &mut String, el: &EnumLayout, ctx: &Ctx) {
    let Some(ref disc) = el.discriminant else {
        return;
    };
    let p = ctx.palette;
    match disc {
        DiscriminantInfo::Direct {
            tag_offset,
            tag_size,
            ..
        } => {
            let _ = writeln!(
                out,
                "  {}discriminant at +0x{:04x}, {} byte(s){}",
                p.dim, tag_offset, tag_size, p.reset
            );
        }
        DiscriminantInfo::Niche {
            tag_offset,
            untagged_variant,
            niche_start,
            ..
        } => {
            let _ = writeln!(
                out,
                "  {}niche discriminant at +0x{:04x}, untagged: {}, niche_start: {}{}",
                p.dim, tag_offset, untagged_variant, niche_start, p.reset
            );
        }
    }
}

fn render_niche_detail(out: &mut String, el: &EnumLayout, ctx: &Ctx) {
    let Some(ref niche) = el.niche else { return };
    let p = ctx.palette;
    let field = niche.field_name.as_deref().unwrap_or("?");
    let _ = writeln!(
        out,
        "  {}niche in `{}` ({}) at +0x{:04x}, valid: 0x{:x}..=0x{:x}{}",
        p.dim,
        field,
        niche.value_type,
        niche.offset,
        niche.valid_range_start,
        niche.valid_range_end,
        p.reset,
    );
}

fn render_variant(out: &mut String, v: &VariantLayout, idx: usize, el: &EnumLayout, ctx: &Ctx) {
    let p = ctx.palette;
    let fc = ctx.fc(idx);
    let tag = variant_tag_suffix(idx, v, el)
        .map(|s| format!(" {}[{s}]{}", p.dim, p.reset))
        .unwrap_or_default();
    if !v.fields.is_empty() {
        let _ = write!(
            out,
            "\n  {}{}{}{}: {}{} bytes{}{tag}\n",
            fc, p.bold, v.name, p.reset, p.dim, v.size, p.reset
        );
        for f in &v.fields {
            let _ = writeln!(
                out,
                "    {}+0x{:04x}{}  {:4}  {}{}: {}{}",
                p.dim, f.offset, p.reset, f.size, fc, f.name, f.typename, p.reset
            );
        }
    } else if v.size > 0 {
        let _ = write!(
            out,
            "\n  {}{}{}{}: {}{} bytes, opaque{}{tag}\n",
            fc, p.bold, v.name, p.reset, p.dim, v.size, p.reset
        );
    } else {
        let _ = write!(
            out,
            "\n  {}{}{}{} {}(unit){}{tag}\n",
            fc, p.bold, v.name, p.reset, p.dim, p.reset
        );
    }
}

fn render_field_table(out: &mut String, fields: &[FieldLayout], total: u64, ctx: &Ctx) {
    let p = ctx.palette;
    let _ = write!(out, "\n  {}Offset    Size  Field{}\n", p.dim, p.reset);
    let _ = writeln!(
        out,
        "  {}────────  ────  ──────────────────────────────────{}",
        p.dim, p.reset
    );
    let mut prev = 0u64;
    for (i, f) in fields.iter().enumerate() {
        if f.offset > prev {
            let _ = writeln!(
                out,
                "  {}0x{prev:06x}  {:4}  ░░░ padding{}",
                p.dim,
                f.offset - prev,
                p.reset
            );
        }
        let _ = writeln!(
            out,
            "  {}0x{:06x}{}  {:4}  {}{}: {}{}",
            p.dim,
            f.offset,
            p.reset,
            f.size,
            ctx.fc(i),
            f.name,
            f.typename,
            p.reset
        );
        if let Some(ref n) = f.largest_niche {
            let _ = writeln!(
                out,
                "  {}                  └─ {} niche{} available ({}, valid 0x{:x}..=0x{:x}){}",
                p.dim,
                format_count(n.available),
                if n.available == 1 { "" } else { "s" },
                n.value_type,
                n.valid_range_start,
                n.valid_range_end,
                p.reset
            );
        }
        prev = f.offset + f.size;
    }
    if prev < total {
        let _ = writeln!(
            out,
            "  {}0x{prev:06x}  {:4}  ░░░ padding{}",
            p.dim,
            total - prev,
            p.reset
        );
    }
    let _ = writeln!(
        out,
        "  {}────────  ────  ──────────────────────────────────{}",
        p.dim, p.reset
    );
    let _ = writeln!(out, "  {}          {:4}  total{}", p.dim, total, p.reset);
}

fn format_count(n: u128) -> String {
    if n >= 1u128 << 60 {
        "a lot".into()
    } else {
        n.to_string()
    }
}

fn render_byte_map(out: &mut String, fields: &[FieldLayout], total: u64, ctx: &Ctx) {
    let t = total as usize;
    if t == 0 || t > 64 || !ctx.color {
        return;
    }
    let p = ctx.palette;
    let bpr = if t <= 8 { 8 } else { 16 };
    let mut labels: Vec<(char, usize)> = vec![('.', usize::MAX); t];
    for (i, f) in fields.iter().enumerate() {
        let ch = f.name.chars().find(|c| c.is_alphanumeric()).unwrap_or('?');
        for b in (f.offset as usize)..((f.offset + f.size) as usize).min(t) {
            labels[b] = (ch, i);
        }
    }
    let _ = write!(out, "\n  {}Byte map:{}\n", p.bold, p.reset);
    for rs in (0..t).step_by(bpr) {
        let re = (rs + bpr).min(t);
        out.push_str("  ");
        for b in rs..re {
            let (ch, fi) = labels[b];
            if ch == '.' {
                let _ = write!(out, "{}░░{} ", p.dim, p.reset);
            } else {
                let _ = write!(out, "{}{}{ch}{ch}{} ", ctx.fc(fi), p.bold, p.reset);
            }
        }
        out.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> Ctx {
        Ctx::new(false, false)
    }

    fn enum_layout_tagged(disc: u128) -> EnumLayout {
        EnumLayout {
            strategy: EnumStrategy::Tagged {
                discriminant_size: 4,
            },
            remaining_niches: None,
            variants: vec![VariantLayout {
                name: "Lit".into(),
                size: 16,
                alignment: 8,
                fields: vec![FieldLayout {
                    name: "0".into(),
                    typename: "f64".into(),
                    offset: 8,
                    size: 8,
                    alignment: 8,
                    children: vec![],
                    largest_niche: None,
                }],
                discr_value: Some(disc),
            }],
            niche: None,
            discriminant: Some(DiscriminantInfo::Direct {
                tag_field_index: 0,
                tag_offset: 0,
                tag_size: 4,
            }),
        }
    }

    #[test]
    fn tagged_shows_tag_value_by_default() {
        let tl = TypeLayout {
            name: "E".into(),
            size: 24,
            alignment: 8,
            hover_info: None,
            largest_niche: None,
            kind: TypeLayoutKind::Enum(enum_layout_tagged(0x2)),
        };
        let out = render_type_layout(&tl, &ctx());
        assert!(out.contains("[tag = 0x2]"), "expected tag suffix: {out}");
    }

    #[test]
    fn niche_optimized_shows_per_variant_values_by_default() {
        let tl = TypeLayout {
            name: "Opt".into(),
            size: 8,
            alignment: 8,
            hover_info: None,
            largest_niche: None,
            kind: TypeLayoutKind::Enum(EnumLayout {
                strategy: EnumStrategy::NicheOptimized {
                    savings: 8,
                    tagged_size: 16,
                },
                remaining_niches: Some(0),
                variants: vec![
                    VariantLayout {
                        name: "Some".into(),
                        size: 8,
                        alignment: 8,
                        fields: vec![],
                        discr_value: None,
                    },
                    VariantLayout {
                        name: "None".into(),
                        size: 0,
                        alignment: 1,
                        fields: vec![],
                        discr_value: None,
                    },
                ],
                niche: None,
                discriminant: Some(DiscriminantInfo::Niche {
                    tag_field_index: 0,
                    tag_offset: 0,
                    untagged_variant: "Some".into(),
                    niche_start: 0,
                    niche_variants_start: 1,
                    niche_variants_end: 1,
                }),
            }),
        };
        let out = render_type_layout(&tl, &ctx());
        assert!(out.contains("[untagged]"), "expected untagged on Some: {out}");
        assert!(out.contains("[niche = 0x0]"), "expected niche on None: {out}");
    }

    #[test]
    fn render_opaque() {
        let tl = TypeLayout {
            name: "X".into(),
            size: 4,
            alignment: 4,
            hover_info: None,
            largest_niche: None,
            kind: TypeLayoutKind::Opaque,
        };
        let out = render_type_layout(&tl, &ctx());
        assert!(out.contains("scalar / opaque"));
    }

    #[test]
    fn render_zst() {
        let tl = TypeLayout {
            name: "()".into(),
            size: 0,
            alignment: 1,
            hover_info: None,
            largest_niche: None,
            kind: TypeLayoutKind::Opaque,
        };
        let out = render_type_layout(&tl, &ctx());
        assert!(out.contains("zero-sized type"));
    }

    #[test]
    fn render_struct_fields_and_padding() {
        let tl = TypeLayout {
            name: "Foo".into(),
            size: 16,
            alignment: 8,
            hover_info: None,
            largest_niche: None,
            kind: TypeLayoutKind::Struct(StructLayout {
                fields: vec![
                    FieldLayout {
                        name: "a".into(),
                        typename: "u64".into(),
                        offset: 0,
                        size: 8,
                        alignment: 8,
                        children: vec![],
                        largest_niche: None,
                    },
                    FieldLayout {
                        name: "b".into(),
                        typename: "u32".into(),
                        offset: 8,
                        size: 4,
                        alignment: 4,
                        children: vec![],
                        largest_niche: None,
                    },
                ],
                padding_bytes: 4,
            }),
        };
        let out = render_type_layout(&tl, &ctx());
        assert!(out.contains("a: u64"));
        assert!(out.contains("b: u32"));
        assert!(out.contains("padding"));
        assert!(out.contains("16  total"));
    }

    #[test]
    fn render_enum_niche_optimized() {
        let tl = TypeLayout {
            name: "Opt".into(),
            size: 8,
            alignment: 8,
            hover_info: None,
            largest_niche: None,
            kind: TypeLayoutKind::Enum(EnumLayout {
                strategy: EnumStrategy::NicheOptimized {
                    savings: 8,
                    tagged_size: 16,
                },
                remaining_niches: Some(42),
                variants: vec![
                    VariantLayout {
                        name: "Some".into(),
                        size: 8,
                        alignment: 8,
                        fields: vec![],
                        discr_value: None,
                    },
                    VariantLayout {
                        name: "None".into(),
                        size: 0,
                        alignment: 1,
                        fields: vec![],
                        discr_value: None,
                    },
                ],
                niche: None,
                discriminant: None,
            }),
        };
        let out = render_type_layout(&tl, &ctx());
        assert!(out.contains("niche optimized"));
    }

    #[test]
    fn render_enum_tagged() {
        let tl = TypeLayout {
            name: "E".into(),
            size: 8,
            alignment: 4,
            hover_info: None,
            largest_niche: None,
            kind: TypeLayoutKind::Enum(EnumLayout {
                strategy: EnumStrategy::Tagged {
                    discriminant_size: 1,
                },
                remaining_niches: None,
                variants: vec![
                    VariantLayout {
                        name: "A".into(),
                        size: 4,
                        alignment: 4,
                        fields: vec![],
                        discr_value: None,
                    },
                    VariantLayout {
                        name: "B".into(),
                        size: 1,
                        alignment: 1,
                        fields: vec![],
                        discr_value: None,
                    },
                ],
                niche: None,
                discriminant: None,
            }),
        };
        let out = render_type_layout(&tl, &ctx());
        assert!(out.contains("tagged enum"));
    }

    #[test]
    fn render_hover_info() {
        let tl = TypeLayout {
            name: "X".into(),
            size: 4,
            alignment: 4,
            hover_info: Some("size = 4, align = 4, no Drop".into()),
            largest_niche: None,
            kind: TypeLayoutKind::Opaque,
        };
        let out = render_type_layout(&tl, &ctx());
        assert!(out.contains("size = 4, align = 4, no Drop"));
    }

    #[test]
    fn render_niche_discriminant_direct() {
        let tl = TypeLayout {
            name: "E".into(),
            size: 8,
            alignment: 4,
            hover_info: None,
            largest_niche: None,
            kind: TypeLayoutKind::Enum(EnumLayout {
                strategy: EnumStrategy::Tagged {
                    discriminant_size: 1,
                },
                remaining_niches: None,
                variants: vec![],
                niche: None,
                discriminant: Some(nichy::DiscriminantInfo::Direct {
                    tag_field_index: 0,
                    tag_offset: 0,
                    tag_size: 1,
                }),
            }),
        };
        let out = render_type_layout(&tl, &ctx());
        assert!(out.contains("discriminant at +0x0000, 1 byte(s)"));
    }

    #[test]
    fn render_niche_discriminant_niche() {
        let tl = TypeLayout {
            name: "Opt".into(),
            size: 8,
            alignment: 8,
            hover_info: None,
            largest_niche: None,
            kind: TypeLayoutKind::Enum(EnumLayout {
                strategy: EnumStrategy::NicheOptimized {
                    savings: 8,
                    tagged_size: 16,
                },
                remaining_niches: Some(0),
                variants: vec![
                    VariantLayout {
                        name: "Some".into(),
                        size: 8,
                        alignment: 8,
                        fields: vec![],
                        discr_value: None,
                    },
                    VariantLayout {
                        name: "None".into(),
                        size: 0,
                        alignment: 1,
                        fields: vec![],
                        discr_value: None,
                    },
                ],
                niche: Some(nichy::NicheInfo {
                    offset: 0,
                    field_name: Some("ptr".into()),
                    value_type: "ptr".into(),
                    value_size: 8,
                    valid_range_start: 1,
                    valid_range_end: u64::MAX as u128,
                    available: 0,
                }),
                discriminant: Some(nichy::DiscriminantInfo::Niche {
                    tag_field_index: 0,
                    tag_offset: 0,
                    untagged_variant: "Some".into(),
                    niche_start: 0,
                    niche_variants_start: 1,
                    niche_variants_end: 1,
                }),
            }),
        };
        let out = render_type_layout(&tl, &ctx());
        assert!(out.contains("niche optimized"));
        assert!(out.contains("niche discriminant at +0x0000"));
        assert!(out.contains("untagged: Some"));
        assert!(out.contains("niche in `ptr`"));
    }

    #[test]
    fn render_struct_largest_niche() {
        let tl = TypeLayout {
            name: "S".into(),
            size: 1,
            alignment: 1,
            hover_info: None,
            largest_niche: Some(nichy::NicheInfo {
                offset: 0,
                field_name: Some("x".into()),
                value_type: "u8".into(),
                value_size: 1,
                valid_range_start: 0,
                valid_range_end: 1,
                available: 254,
            }),
            kind: TypeLayoutKind::Struct(StructLayout {
                fields: vec![FieldLayout {
                    name: "x".into(),
                    typename: "bool".into(),
                    offset: 0,
                    size: 1,
                    alignment: 1,
                    children: vec![],
                    largest_niche: Some(nichy::NicheInfo {
                        offset: 0,
                        field_name: Some("x".into()),
                        value_type: "u8".into(),
                        value_size: 1,
                        valid_range_start: 0,
                        valid_range_end: 1,
                        available: 254,
                    }),
                }],
                padding_bytes: 0,
            }),
        };
        let out = render_type_layout(&tl, &ctx());
        assert!(out.contains("niche: `x` (u8) at +0x0000, 254 available"));
        assert!(out.contains("254 niches available (u8, valid 0x0..=0x1)"));
    }
}
