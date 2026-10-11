use homesense_core::{
    rule::{Value, ValueType::*},
    variable_migration::{Conversion::*, Error},
};
#[test]
fn exact_conversions_preserve_unknown_and_reject_lossy_values() {
    for (boolean, integer) in [(false, 0), (true, 1)] {
        assert_eq!(
            BoolToInteger.apply(Bool, Integer, Some(&Value::Bool(boolean))),
            Ok(Some(Value::Integer(integer)))
        );
        assert_eq!(
            IntegerZeroOneToBool.apply(Integer, Bool, Some(&Value::Integer(integer))),
            Ok(Some(Value::Bool(boolean)))
        );
    }
    for value in [i64::MIN, -1, 2, i64::MAX] {
        assert_eq!(
            IntegerZeroOneToBool.apply(Integer, Bool, Some(&Value::Integer(value))),
            Err(Error::NonBooleanInteger)
        );
    }
    assert_eq!(BoolToInteger.apply(Bool, Integer, None), Ok(None));
    assert_eq!(IntegerZeroOneToBool.apply(Integer, Bool, None), Ok(None));
}
#[test]
fn direction_and_source_type_are_checked_even_when_resetting() {
    assert_eq!(
        BoolToInteger.apply(Integer, Bool, None),
        Err(Error::WrongDirection)
    );
    assert_eq!(
        IntegerZeroOneToBool.apply(Bool, Integer, None),
        Err(Error::WrongDirection)
    );
    for conversion in [BoolToInteger, IntegerZeroOneToBool, ResetUnknown] {
        assert_eq!(
            conversion.apply(Bool, Integer, Some(&Value::Integer(1))),
            Err(Error::SourceTypeMismatch)
        );
    }
    assert_eq!(
        ResetUnknown.apply(Integer, Bool, Some(&Value::Integer(42))),
        Ok(None)
    );
    assert_eq!(
        ResetUnknown.apply(Bool, Bool, Some(&Value::Bool(true))),
        Ok(None)
    );
}
