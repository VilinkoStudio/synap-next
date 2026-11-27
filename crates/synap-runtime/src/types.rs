//! 声明里用到的名字与类型。

use std::fmt;
use std::str::FromStr;

use synap_core::{SchemaId, TraitId};

use crate::error::PluginError;

/// 带大版本的名字：组件引用、Schema 引用、扩展目标都用它。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct VersionedName {
    /// 稳定名义身份，不含 `@`。
    pub name: SchemaId,
    /// 大版本。
    pub major: u16,
}

impl VersionedName {
    /// 解析 `namespace/name@major`。
    pub fn parse(text: &str) -> Result<Self, PluginError> {
        let (name, major) = text
            .rsplit_once('@')
            .ok_or_else(|| PluginError::manifest(format!("`{text}` must end with @major")))?;
        let major = major
            .parse::<u16>()
            .map_err(|_| PluginError::manifest(format!("`{text}` has a bad major version")))?;
        let name = SchemaId::parse(name)
            .map_err(|_| PluginError::manifest(format!("`{name}` is not namespace/name")))?;
        Ok(Self { name, major })
    }

    /// 声明处允许省略版本，默认 `@1`。
    pub fn parse_decl(text: &str) -> Result<Self, PluginError> {
        if text.rsplit_once('@').is_some() {
            Self::parse(text)
        } else {
            Self::parse(&format!("{text}@1"))
        }
    }
}

impl fmt::Display for VersionedName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.name, self.major)
    }
}

/// Trait 引用必须带大版本。
pub fn parse_trait(text: &str) -> Result<TraitId, PluginError> {
    let (name, major) = text
        .rsplit_once('@')
        .ok_or_else(|| PluginError::manifest(format!("trait `{text}` must end with @major")))?;
    let major = major
        .parse::<u16>()
        .map_err(|_| PluginError::manifest(format!("trait `{text}` has a bad major version")))?;
    TraitId::parse(name, major)
        .map_err(|_| PluginError::manifest(format!("trait `{name}` is not namespace/name")))
}

/// 字段类型。类型字符串是插件作者写的声明，不是 Rust 类型系统；
/// 认不出的字符串在加载时报错（拼错的类型不能悄悄关掉校验），要不校验就写 `any`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TypeSpec {
    /// 不做额外校验。
    Any,
    /// 布尔。
    Bool,
    /// 整数，带范围。
    Int(IntType),
    /// 浮点。整数写进浮点字段会提升为浮点。
    Float,
    /// UTF-8 文本。
    Text,
    /// 不透明字节。
    Bytes,
    /// 实体引用。
    Ref,
    /// 大二进制摘要。
    Blob,
    /// 混合逻辑时钟。`time` 也落到这里，v1 没有独立的墙上时间类型。
    Hlc,
    /// 列表。
    List(Box<TypeSpec>),
    /// 可以为 `null`。
    Optional(Box<TypeSpec>),
}

/// 整数宽度。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntType {
    /// `u8`
    U8,
    /// `u16`
    U16,
    /// `u32`
    U32,
    /// `u64`（受 LuaJIT 的 2^53 精确范围限制）
    U64,
    /// `i8`
    I8,
    /// `i16`
    I16,
    /// `i32`
    I32,
    /// `i64`
    I64,
}

impl IntType {
    /// 允许的范围。
    pub fn bounds(self) -> (i64, i64) {
        match self {
            Self::U8 => (0, u8::MAX as i64),
            Self::U16 => (0, u16::MAX as i64),
            Self::U32 => (0, u32::MAX as i64),
            Self::U64 => (0, i64::MAX),
            Self::I8 => (i8::MIN as i64, i8::MAX as i64),
            Self::I16 => (i16::MIN as i64, i16::MAX as i64),
            Self::I32 => (i32::MIN as i64, i32::MAX as i64),
            Self::I64 => (i64::MIN, i64::MAX),
        }
    }
}

impl FromStr for TypeSpec {
    type Err = PluginError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let text = text.trim();
        if let Some(inner) = text.strip_suffix('?') {
            return Ok(Self::Optional(Box::new(inner.parse()?)));
        }
        if let Some(inner) = text.strip_prefix('[').and_then(|t| t.strip_suffix(']')) {
            return Ok(Self::List(Box::new(inner.parse()?)));
        }
        Ok(match text {
            "bool" => Self::Bool,
            "u8" => Self::Int(IntType::U8),
            "u16" => Self::Int(IntType::U16),
            "u32" => Self::Int(IntType::U32),
            "u64" => Self::Int(IntType::U64),
            "i8" => Self::Int(IntType::I8),
            "i16" => Self::Int(IntType::I16),
            "i32" => Self::Int(IntType::I32),
            "i64" | "int" => Self::Int(IntType::I64),
            "float" | "f64" => Self::Float,
            "text" | "string" => Self::Text,
            "bytes" => Self::Bytes,
            "ref" => Self::Ref,
            "blob" => Self::Blob,
            "hlc" | "time" => Self::Hlc,
            "any" => Self::Any,
            other => {
                return Err(PluginError::manifest(format!(
                    "unknown type `{other}`; use one of bool, u8..u64, i8..i64, int, float, \
                     text, bytes, ref, blob, time, any, [T], T?"
                )))
            }
        })
    }
}

impl TypeSpec {
    /// 类型字符串，和声明里的写法一致。
    pub fn describe(&self) -> String {
        match self {
            Self::Any => "any".to_owned(),
            Self::Bool => "bool".to_owned(),
            Self::Int(kind) => match kind {
                IntType::U8 => "u8",
                IntType::U16 => "u16",
                IntType::U32 => "u32",
                IntType::U64 => "u64",
                IntType::I8 => "i8",
                IntType::I16 => "i16",
                IntType::I32 => "i32",
                IntType::I64 => "i64",
            }
            .to_owned(),
            Self::Float => "float".to_owned(),
            Self::Text => "text".to_owned(),
            Self::Bytes => "bytes".to_owned(),
            Self::Ref => "ref".to_owned(),
            Self::Blob => "blob".to_owned(),
            Self::Hlc => "hlc".to_owned(),
            Self::List(inner) => format!("[{}]", inner.describe()),
            Self::Optional(inner) => format!("{}?", inner.describe()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versioned_names_need_versions_in_references() {
        assert!(VersionedName::parse("std/Title").is_err());
        let name = VersionedName::parse("std/Title@2").unwrap();
        assert_eq!(name.major, 2);
        assert_eq!(name.to_string(), "std/Title@2");
        assert_eq!(VersionedName::parse_decl("std/Title").unwrap().major, 1);
    }

    #[test]
    fn type_strings_nest() {
        assert_eq!(
            "[ref]?".parse::<TypeSpec>().unwrap(),
            TypeSpec::Optional(Box::new(TypeSpec::List(Box::new(TypeSpec::Ref))))
        );
        assert_eq!(
            "u8".parse::<TypeSpec>().unwrap(),
            TypeSpec::Int(IntType::U8)
        );
        assert_eq!("time".parse::<TypeSpec>().unwrap(), TypeSpec::Hlc);
        assert_eq!("any".parse::<TypeSpec>().unwrap(), TypeSpec::Any);
        assert!("weird".parse::<TypeSpec>().is_err());
        assert!("integer".parse::<TypeSpec>().is_err());
    }
}
