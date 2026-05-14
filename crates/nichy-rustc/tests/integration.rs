#![feature(rustc_private)]

extern crate rustc_driver;

use nichy::{DiscriminantInfo, EnumStrategy, TypeLayoutKind};

#[test]
fn analyze_struct_basic() {
    let layouts =
        nichy_rustc::analyze_snippet("struct Foo { a: u8, b: u64, c: u8 }", None, None).unwrap();
    assert_eq!(layouts.len(), 1);
    let tl = &layouts[0];
    assert_eq!(tl.name, "Foo");
    assert_eq!(tl.size, 16);
    assert_eq!(tl.alignment, 8);
    let TypeLayoutKind::Struct(ref sl) = tl.kind else {
        panic!("expected struct")
    };
    assert_eq!(sl.fields.len(), 3);
    assert_eq!(sl.fields[0].name, "b");
    assert_eq!(sl.fields[0].offset, 0);
}

#[test]
fn analyze_type_expr_option_ref() {
    let layouts = nichy_rustc::analyze_type_expr("Option<&u64>", None, None).unwrap();
    assert_eq!(layouts.len(), 1);
    let tl = &layouts[0];
    assert_eq!(tl.size, 8);
    let TypeLayoutKind::Enum(ref el) = tl.kind else {
        panic!("expected enum")
    };
    assert!(matches!(el.strategy, EnumStrategy::NicheOptimized { .. }));
    assert!(el.discriminant.is_some());
    let Some(DiscriminantInfo::Niche {
        ref untagged_variant,
        niche_start,
        ..
    }) = el.discriminant
    else {
        panic!("expected niche discriminant")
    };
    assert_eq!(untagged_variant, "Some");
    assert_eq!(niche_start, 0);
}

#[test]
fn analyze_type_expr_option_bool() {
    let layouts = nichy_rustc::analyze_type_expr("Option<bool>", None, None).unwrap();
    assert_eq!(layouts.len(), 1);
    let tl = &layouts[0];
    assert_eq!(tl.size, 1);
    let TypeLayoutKind::Enum(ref el) = tl.kind else {
        panic!("expected enum")
    };
    assert!(matches!(el.strategy, EnumStrategy::NicheOptimized { .. }));
    assert!(el.niche.is_some());
    let niche = el.niche.as_ref().unwrap();
    assert_eq!(niche.value_type, "u8");
    assert_eq!(niche.offset, 0);
    assert!(el.remaining_niches.unwrap() > 0);
}

#[test]
fn analyze_tagged_enum() {
    let layouts = nichy_rustc::analyze_type_expr("Result<u32, bool>", None, None).unwrap();
    assert_eq!(layouts.len(), 1);
    let tl = &layouts[0];
    assert_eq!(tl.size, 8);
    let TypeLayoutKind::Enum(ref el) = tl.kind else {
        panic!("expected enum")
    };
    assert!(matches!(
        el.strategy,
        EnumStrategy::Tagged {
            discriminant_size: 1
        }
    ));
    assert!(el.discriminant.is_some());
    let Some(DiscriminantInfo::Direct { tag_size, .. }) = el.discriminant else {
        panic!("expected direct discriminant")
    };
    assert_eq!(tag_size, 1);
    assert_eq!(el.variants.len(), 2);
}

#[test]
fn analyze_snippet_multiple_types() {
    let layouts =
        nichy_rustc::analyze_snippet("struct A(u8);\nstruct B(u64);\nenum C { X, Y }", None, None)
            .unwrap();
    assert_eq!(layouts.len(), 3);
    let names: Vec<&str> = layouts.iter().map(|l| l.name.as_str()).collect();
    assert!(names.contains(&"A"));
    assert!(names.contains(&"B"));
    assert!(names.contains(&"C"));
}

#[test]
fn analyze_zst() {
    let layouts = nichy_rustc::analyze_snippet("struct Zst;", None, None).unwrap();
    assert_eq!(layouts.len(), 1);
    assert_eq!(layouts[0].size, 0);
    assert_eq!(layouts[0].alignment, 1);
}

#[test]
fn analyze_enum_with_fields() {
    let layouts = nichy_rustc::analyze_snippet(
        "enum Shape { Circle(f64), Rect { w: f64, h: f64 }, Point }",
        None,
        None,
    )
    .unwrap();
    assert_eq!(layouts.len(), 1);
    let tl = &layouts[0];
    let TypeLayoutKind::Enum(ref el) = tl.kind else {
        panic!("expected enum")
    };
    assert_eq!(el.variants.len(), 3);
    let circle = el.variants.iter().find(|v| v.name == "Circle").unwrap();
    assert_eq!(circle.fields.len(), 1);
    assert_eq!(circle.fields[0].typename, "f64");
    let rect = el.variants.iter().find(|v| v.name == "Rect").unwrap();
    assert_eq!(rect.fields.len(), 2);
}

#[test]
fn analyze_skips_generic_types() {
    let layouts =
        nichy_rustc::analyze_snippet("struct Concrete(u32);\nstruct Generic<T>(T);", None, None)
            .unwrap();
    assert_eq!(layouts.len(), 1);
    assert_eq!(layouts[0].name, "Concrete");
}

#[test]
fn analyze_nonzero_niche() {
    let layouts = nichy_rustc::analyze_snippet(
        "use std::num::NonZeroU64;\nenum Token { Id(NonZeroU64), Anon }",
        None,
        None,
    )
    .unwrap();
    assert_eq!(layouts.len(), 1);
    let tl = &layouts[0];
    assert_eq!(tl.size, 8);
    let TypeLayoutKind::Enum(ref el) = tl.kind else {
        panic!("expected enum")
    };
    assert!(matches!(el.strategy, EnumStrategy::NicheOptimized { .. }));
    let Some(DiscriminantInfo::Niche { niche_start, .. }) = el.discriminant else {
        panic!("expected niche discriminant")
    };
    assert_eq!(niche_start, 0);
}

#[test]
fn analyze_field_niches() {
    let layouts = nichy_rustc::analyze_snippet(
        "use std::num::NonZeroU8;\nstruct S { a: bool, b: NonZeroU8, c: u8 }",
        None,
        None,
    )
    .unwrap();
    let tl = &layouts[0];
    let TypeLayoutKind::Struct(ref sl) = tl.kind else {
        panic!("expected struct")
    };
    let by_name = |n: &str| sl.fields.iter().find(|f| f.name == n).unwrap();

    let a = by_name("a");
    assert!(
        a.largest_niche.is_some(),
        "bool field should expose a niche"
    );
    assert_eq!(a.largest_niche.as_ref().unwrap().available, 254);

    let b = by_name("b");
    assert!(
        b.largest_niche.is_some(),
        "NonZeroU8 field should expose a niche"
    );
    assert_eq!(b.largest_niche.as_ref().unwrap().available, 1);

    let c = by_name("c");
    assert!(c.largest_niche.is_none(), "plain u8 has no niche");
}

#[test]
fn analyze_compilation_error_returns_err() {
    let result =
        nichy_rustc::analyze_snippet("fn main() { let x: u32 = \"not a u32\"; }", None, None);
    assert!(result.is_err());
}
