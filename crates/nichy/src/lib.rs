mod serde_u128 {
    use serde::{self, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(val: &u128, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&val.to_string())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u128, D::Error> {
        struct Visitor;
        impl serde::de::Visitor<'_> for Visitor {
            type Value = u128;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a u128 as string or number")
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<u128, E> {
                v.parse().map_err(E::custom)
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<u128, E> {
                Ok(v as u128)
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<u128, E> {
                Ok(v as u128)
            }
        }
        d.deserialize_any(Visitor)
    }
}

mod serde_u128_opt {
    use serde::{self, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(val: &Option<u128>, s: S) -> Result<S::Ok, S::Error> {
        match val {
            Some(v) => s.serialize_str(&v.to_string()),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u128>, D::Error> {
        struct Visitor;
        impl serde::de::Visitor<'_> for Visitor {
            type Value = Option<u128>;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("null, or a u128 as string or number")
            }
            fn visit_none<E: serde::de::Error>(self) -> Result<Option<u128>, E> {
                Ok(None)
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Option<u128>, E> {
                Ok(None)
            }
            fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Option<u128>, E> {
                v.parse().map(Some).map_err(E::custom)
            }
            fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Option<u128>, E> {
                Ok(Some(v as u128))
            }
            fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Option<u128>, E> {
                Ok(Some(v as u128))
            }
        }
        d.deserialize_any(Visitor)
    }
}

pub const PREAMBLE: &str = "\
#![allow(dead_code, unused_imports)]\n\
use std::num::*;\n\
use std::ptr::NonNull;\n\
use std::cell::{Cell, RefCell, UnsafeCell};\n\
use std::sync::{Arc, Mutex, RwLock};\n\
use std::rc::Rc;\n\
use std::collections::*;\n\
use std::ffi::{CStr, CString, OsStr, OsString};\n\
use std::path::{Path, PathBuf};\n\
\n";

// Splits leading inner attributes (`#![...]`) from the rest of a source file
// so we can stitch them above an injected preamble. Inner attributes must
// precede all items, so anything after the first real item stays in `body`.
pub fn split_inner_attrs(code: &str) -> (String, String) {
    let mut attrs = String::new();
    let mut body = String::new();
    let mut scanning = true;
    for line in code.lines() {
        if scanning {
            let trimmed = line.trim();
            if trimmed.starts_with("#![") {
                attrs.push_str(line);
                attrs.push('\n');
                continue;
            }
            if !trimmed.is_empty() && !trimmed.starts_with("//") {
                scanning = false;
            }
        }
        body.push_str(line);
        body.push('\n');
    }
    (attrs, body)
}

// Number of leading inner-attribute lines, using the same scanning rules as
// split_inner_attrs. Keeping them in lockstep keeps the web crate's rustc-error
// line-number rewriting aligned with the snippet wrapper.
pub fn count_inner_attr_lines(code: &str) -> usize {
    let mut count = 0;
    for line in code.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("#![") {
            count += 1;
        } else if !trimmed.is_empty() && !trimmed.starts_with("//") {
            break;
        }
    }
    count
}

// ── precise niche types ────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NicheInfo {
    pub offset: u64,
    pub field_name: Option<String>,
    pub value_type: String,
    pub value_size: u64,
    #[serde(with = "serde_u128")]
    pub valid_range_start: u128,
    #[serde(with = "serde_u128")]
    pub valid_range_end: u128,
    #[serde(with = "serde_u128")]
    pub available: u128,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "encoding")]
pub enum DiscriminantInfo {
    Direct {
        tag_field_index: usize,
        tag_offset: u64,
        tag_size: u64,
    },
    Niche {
        tag_field_index: usize,
        tag_offset: u64,
        untagged_variant: String,
        #[serde(with = "serde_u128")]
        niche_start: u128,
        niche_variants_start: usize,
        niche_variants_end: usize,
    },
}

// ── layout types ───────────────────────────────────────────

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TypeLayout {
    pub name: String,
    pub size: u64,
    pub alignment: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hover_info: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub largest_niche: Option<NicheInfo>,
    #[serde(flatten)]
    pub kind: TypeLayoutKind,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind")]
pub enum TypeLayoutKind {
    #[serde(rename = "struct")]
    Struct(StructLayout),
    #[serde(rename = "enum")]
    Enum(EnumLayout),
    #[serde(rename = "opaque")]
    Opaque,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StructLayout {
    pub fields: Vec<FieldLayout>,
    pub padding_bytes: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FieldLayout {
    pub name: String,
    pub typename: String,
    pub offset: u64,
    pub size: u64,
    pub alignment: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<FieldLayout>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub largest_niche: Option<NicheInfo>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EnumLayout {
    pub strategy: EnumStrategy,
    #[serde(default, with = "serde_u128_opt")]
    pub remaining_niches: Option<u128>,
    pub variants: Vec<VariantLayout>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub niche: Option<NicheInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub discriminant: Option<DiscriminantInfo>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub enum EnumStrategy {
    Single,
    NicheOptimized { savings: u64, tagged_size: u64 },
    Tagged { discriminant_size: u64 },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct VariantLayout {
    pub name: String,
    pub size: u64,
    pub alignment: u64,
    pub fields: Vec<FieldLayout>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "serde_u128_opt"
    )]
    pub discr_value: Option<u128>,
}

#[cfg(test)]
mod tests {
    use super::{count_inner_attr_lines, split_inner_attrs};

    #[test]
    fn split_extracts_feature() {
        let code = "#![feature(never_type)]\n\nstruct Foo;\n";
        let (attrs, body) = split_inner_attrs(code);
        assert_eq!(attrs, "#![feature(never_type)]\n");
        assert_eq!(body, "\nstruct Foo;\n");
    }

    #[test]
    fn split_multiple_attrs() {
        let code = "#![feature(never_type)]\n#![feature(generic_const_exprs)]\n\nstruct Foo;\n";
        let (attrs, body) = split_inner_attrs(code);
        assert_eq!(
            attrs,
            "#![feature(never_type)]\n#![feature(generic_const_exprs)]\n"
        );
        assert_eq!(body, "\nstruct Foo;\n");
    }

    #[test]
    fn split_no_attrs() {
        let code = "struct Foo;\n";
        let (attrs, body) = split_inner_attrs(code);
        assert_eq!(attrs, "");
        assert_eq!(body, "struct Foo;\n");
    }

    #[test]
    fn split_comment_between_attrs() {
        let code = "#![feature(never_type)]\n// a comment\n#![allow(unused)]\nstruct Foo;\n";
        let (attrs, body) = split_inner_attrs(code);
        assert_eq!(attrs, "#![feature(never_type)]\n#![allow(unused)]\n");
        assert_eq!(body, "// a comment\nstruct Foo;\n");
    }

    #[test]
    fn split_stops_at_item() {
        let code = "#![feature(never_type)]\nstruct Foo;\n#![allow(unused)]\n";
        let (attrs, body) = split_inner_attrs(code);
        assert_eq!(attrs, "#![feature(never_type)]\n");
        assert_eq!(body, "struct Foo;\n#![allow(unused)]\n");
    }

    #[test]
    fn count_matches_split() {
        let cases = [
            "#![feature(never_type)]\n\nstruct Foo;\n",
            "#![a]\n#![b]\n\nstruct Foo;\n",
            "struct Foo;\n",
            "#![a]\n// c\n#![b]\nstruct Foo;\n",
            "#![a]\nstruct Foo;\n#![b]\n",
        ];
        for code in cases {
            let (attrs, _) = split_inner_attrs(code);
            assert_eq!(
                count_inner_attr_lines(code),
                attrs.lines().count(),
                "case: {code:?}"
            );
        }
    }
}
