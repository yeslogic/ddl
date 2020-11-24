use fathom_runtime::{FormatReader, ReadError, ReadFormat};
use num_traits::ToPrimitive;
use std::collections::HashMap;
use std::sync::Arc;

use crate::lang::core;
use crate::lang::core::semantics::{self, Elim, Head, Value};
use crate::lang::core::{Globals, Item, ItemData, Module, Primitive, StructFormat};

/// Contextual information to be used when parsing items.
pub struct Context<'me> {
    globals: &'me Globals,
    items: HashMap<String, Item>,
    reader: FormatReader<'me>,
}

impl<'me> Context<'me> {
    /// Create a new context.
    pub fn new(globals: &'me Globals, reader: FormatReader<'me>) -> Context<'me> {
        Context {
            globals,
            items: HashMap::new(),
            reader,
        }
    }

    /// Evaluate a term in the parser context.
    fn eval(&self, term: &core::Term) -> Arc<Value> {
        semantics::eval(self.globals, &self.items, term)
    }

    /// Read a module item in the context.
    pub fn read_item(&mut self, module: &'me Module, name: &str) -> Result<Value, ReadError> {
        for item in &module.items {
            let name = match &item.data {
                ItemData::Constant(constant) if constant.name == name => {
                    let value = self.eval(&constant.term);
                    return self.read_format(&value);
                }
                ItemData::StructFormat(struct_format) if struct_format.name == name => {
                    return self.read_struct_format(struct_format);
                }
                ItemData::Constant(constant) => constant.name.clone(),
                ItemData::StructType(struct_type) => struct_type.name.clone(),
                ItemData::StructFormat(struct_format) => struct_format.name.clone(),
            };
            self.items.insert(name, item.clone());
        }

        Err(ReadError::InvalidDataDescription)
    }

    fn read<T: ReadFormat<'me>>(&mut self) -> Result<T::Host, ReadError> {
        self.reader.read::<T>()
    }

    fn read_struct_format(&mut self, struct_type: &StructFormat) -> Result<Value, ReadError> {
        let fields = struct_type
            .fields
            .iter()
            .map(|field_declaration| {
                let value = self.eval(&field_declaration.term);
                Ok((
                    field_declaration.label.data.clone(),
                    Arc::new(self.read_format(&value)?),
                ))
            })
            .collect::<Result<_, ReadError>>()?;

        Ok(Value::StructTerm(fields))
    }

    fn read_format(&mut self, format: &Value) -> Result<Value, ReadError> {
        match format {
            Value::Stuck(Head::Global(name), elims) => match (name.as_str(), elims.as_slice()) {
                ("U8", []) => Ok(Value::int(self.read::<fathom_runtime::U8>()?)),
                ("U16Le", []) => Ok(Value::int(self.read::<fathom_runtime::U16Le>()?)),
                ("U16Be", []) => Ok(Value::int(self.read::<fathom_runtime::U16Be>()?)),
                ("U32Le", []) => Ok(Value::int(self.read::<fathom_runtime::U32Le>()?)),
                ("U32Be", []) => Ok(Value::int(self.read::<fathom_runtime::U32Be>()?)),
                ("U64Le", []) => Ok(Value::int(self.read::<fathom_runtime::U64Le>()?)),
                ("U64Be", []) => Ok(Value::int(self.read::<fathom_runtime::U64Be>()?)),
                ("S8", []) => Ok(Value::int(self.read::<fathom_runtime::I8>()?)),
                ("S16Le", []) => Ok(Value::int(self.read::<fathom_runtime::I16Le>()?)),
                ("S16Be", []) => Ok(Value::int(self.read::<fathom_runtime::I16Be>()?)),
                ("S32Le", []) => Ok(Value::int(self.read::<fathom_runtime::I32Le>()?)),
                ("S32Be", []) => Ok(Value::int(self.read::<fathom_runtime::I32Be>()?)),
                ("S64Le", []) => Ok(Value::int(self.read::<fathom_runtime::I64Le>()?)),
                ("S64Be", []) => Ok(Value::int(self.read::<fathom_runtime::I64Be>()?)),
                ("F32Le", []) => Ok(Value::f32(self.read::<fathom_runtime::F32Le>()?)),
                ("F32Be", []) => Ok(Value::f32(self.read::<fathom_runtime::F32Be>()?)),
                ("F64Le", []) => Ok(Value::f64(self.read::<fathom_runtime::F64Le>()?)),
                ("F64Be", []) => Ok(Value::f64(self.read::<fathom_runtime::F64Be>()?)),
                ("FormatArray", [Elim::Function(len), Elim::Function(elem_type)]) => {
                    match len.as_ref() {
                        Value::Primitive(Primitive::Int(len)) => match len.to_usize() {
                            Some(len) => Ok(Value::ArrayTerm(
                                (0..len)
                                    .map(|_| Ok(Arc::new(self.read_format(elem_type)?)))
                                    .collect::<Result<_, ReadError>>()?,
                            )),
                            None => Err(ReadError::InvalidDataDescription),
                        },
                        _ => Err(ReadError::InvalidDataDescription),
                    }
                }
                (_, _) => Err(ReadError::InvalidDataDescription),
            },
            Value::Stuck(Head::Item(name), elims) => {
                match (self.items.get(name.as_str()).cloned(), elims.as_slice()) {
                    (Some(item), []) => match item.data {
                        ItemData::StructFormat(struct_format) => {
                            self.read_struct_format(&struct_format)
                        }
                        // NOTE: We expect that all constants should be reduced
                        // during evaluation, but this assumption could be
                        // invalidated if we ever introduce 'opaque' constants.
                        ItemData::Constant(_) | ItemData::StructType(_) => {
                            Err(ReadError::InvalidDataDescription)
                        }
                    },
                    (Some(_), _) | (None, _) => Err(ReadError::InvalidDataDescription),
                }
            }
            Value::Stuck(Head::Error, _)
            | Value::Sort(_)
            | Value::FunctionType(_, _)
            | Value::StructTerm(_)
            | Value::ArrayTerm(_)
            | Value::Primitive(_)
            | Value::FormatType
            | Value::Repr
            | Value::Error => Err(ReadError::InvalidDataDescription),
        }
    }
}
