//! Plain-data marshalling between [`serde_json::Value`] and native Python
//! objects.
//!
//! The Python engine, like the deno engine, keeps only plain data crossing the
//! Rust<->host boundary: an event handed to a Python hook is a `dict`/`list`/…
//! built from the event's JSON, and a handler's return is read back into a
//! [`Value`] before it is deserialized into the typed host record. These two
//! helpers are that marshalling — the PyO3 analog of `JSON.parse`/`JSON.stringify`
//! at the deno boundary. No PyO3 handle is ever stored in a record; only owned
//! Rust data survives a [`Python::with_gil`](pyo3::Python::with_gil) block.

use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PyString, PyTuple};
use serde_json::{Map, Number, Value};

/// Build a native Python object from a [`Value`] (the JSON→Python direction).
///
/// `null`→`None`, `bool`→`bool`, integral numbers→`int`, other numbers→`float`,
/// strings→`str`, arrays→`list`, objects→`dict`. Mirrors what an extension author
/// sees when a hook receives `event` / a tool receives its `args`.
pub fn json_to_py<'py>(py: Python<'py>, value: &Value) -> PyResult<Bound<'py, PyAny>> {
    match value {
        Value::Null => Ok(py.None().into_bound(py)),
        Value::Bool(b) => Ok(PyBool::new(py, *b).to_owned().into_any()),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Ok(i.into_pyobject(py)?.into_any())
            } else if let Some(u) = n.as_u64() {
                Ok(u.into_pyobject(py)?.into_any())
            } else {
                Ok(PyFloat::new(py, n.as_f64().unwrap_or(0.0)).into_any())
            }
        }
        Value::String(s) => Ok(PyString::new(py, s).into_any()),
        Value::Array(items) => {
            let list = PyList::empty(py);
            for item in items {
                list.append(json_to_py(py, item)?)?;
            }
            Ok(list.into_any())
        }
        Value::Object(map) => {
            let dict = PyDict::new(py);
            for (key, val) in map {
                dict.set_item(key, json_to_py(py, val)?)?;
            }
            Ok(dict.into_any())
        }
    }
}

/// Read a native Python object back into a [`Value`] (the Python→JSON direction).
///
/// `None`→`null`, `bool`→`bool` (checked BEFORE `int`, since `bool` subclasses
/// `int` in Python), `int`→integral number, `float`→number, `str`→string,
/// `list`/`tuple`→array, `dict`→object. Any other object is stringified via its
/// `str()` (the lossy fallback pi's `JSON.stringify` has no analog for, but which
/// keeps an unexpected return from unwinding the host).
pub fn py_to_json(ob: &Bound<'_, PyAny>) -> PyResult<Value> {
    if ob.is_none() {
        return Ok(Value::Null);
    }
    // `bool` must be tested before `int`: a Python `bool` is a subclass of `int`,
    // so an `int` downcast would also succeed and mis-encode `True` as `1`.
    if let Ok(b) = ob.downcast::<PyBool>() {
        return Ok(Value::Bool(b.is_true()));
    }
    if let Ok(s) = ob.downcast::<PyString>() {
        return Ok(Value::String(s.to_str()?.to_owned()));
    }
    if let Ok(i) = ob.downcast::<PyInt>() {
        return Ok(Value::Number(i.extract::<i64>()?.into()));
    }
    if let Ok(f) = ob.downcast::<PyFloat>() {
        let number = Number::from_f64(f.extract::<f64>()?).unwrap_or_else(|| 0.into());
        return Ok(Value::Number(number));
    }
    if let Ok(list) = ob.downcast::<PyList>() {
        let mut array = Vec::with_capacity(list.len());
        for item in list.iter() {
            array.push(py_to_json(&item)?);
        }
        return Ok(Value::Array(array));
    }
    if let Ok(tuple) = ob.downcast::<PyTuple>() {
        let mut array = Vec::with_capacity(tuple.len());
        for item in tuple.iter() {
            array.push(py_to_json(&item)?);
        }
        return Ok(Value::Array(array));
    }
    if let Ok(dict) = ob.downcast::<PyDict>() {
        let mut map = Map::new();
        for (key, val) in dict.iter() {
            map.insert(key.str()?.to_str()?.to_owned(), py_to_json(&val)?);
        }
        return Ok(Value::Object(map));
    }
    Ok(Value::String(ob.str()?.to_str()?.to_owned()))
}
