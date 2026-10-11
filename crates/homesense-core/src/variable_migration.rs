//! Explicit pure conversions for publication-time persistent variable migration.
//! Callers must validate the source revision/type and commit the entire migration
//! with the configuration transaction. This module never writes or enables rules.
use crate::rule::{Value, ValueType};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Conversion {
    BoolToInteger,
    IntegerZeroOneToBool,
    ResetUnknown,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    WrongDirection,
    SourceTypeMismatch,
    NonBooleanInteger,
}
impl Conversion {
    pub fn apply(
        self,
        source: ValueType,
        target: ValueType,
        value: Option<&Value>,
    ) -> Result<Option<Value>, Error> {
        if value.is_some_and(|value| {
            !matches!(
                (source, value),
                (ValueType::Bool, Value::Bool(_)) | (ValueType::Integer, Value::Integer(_))
            )
        }) {
            return Err(Error::SourceTypeMismatch);
        }
        match self {
            Self::BoolToInteger if (source, target) != (ValueType::Bool, ValueType::Integer) => {
                return Err(Error::WrongDirection);
            }
            Self::IntegerZeroOneToBool
                if (source, target) != (ValueType::Integer, ValueType::Bool) =>
            {
                return Err(Error::WrongDirection);
            }
            Self::ResetUnknown => return Ok(None),
            _ => {}
        }
        Ok(match (self, value) {
            (_, None) => None,
            (Self::BoolToInteger, Some(Value::Bool(value))) => {
                Some(Value::Integer(i64::from(*value)))
            }
            (Self::IntegerZeroOneToBool, Some(Value::Integer(0))) => Some(Value::Bool(false)),
            (Self::IntegerZeroOneToBool, Some(Value::Integer(1))) => Some(Value::Bool(true)),
            (Self::IntegerZeroOneToBool, Some(Value::Integer(_))) => {
                return Err(Error::NonBooleanInteger);
            }
            _ => return Err(Error::SourceTypeMismatch),
        })
    }
}
