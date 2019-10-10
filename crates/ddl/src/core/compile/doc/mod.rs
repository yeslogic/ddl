use codespan::FileId;
use codespan_reporting::diagnostic::Diagnostic;
use std::borrow::Cow;
use std::collections::HashMap;
use std::io;
use std::io::prelude::*;

use crate::core;

pub fn compile_module(
    writer: &mut impl Write,
    module: &core::Module,
    report: &mut dyn FnMut(Diagnostic),
) -> io::Result<()> {
    let mut context = ModuleContext {
        _file_id: module.file_id,
        items: HashMap::new(),
    };

    write!(
        writer,
        r##"<!--
  This file is automatically @generated by {pkg_name} {pkg_version}
  It is not intended for manual editing.
-->

<!DOCTYPE html>
<html lang="en">
  <head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <meta http-equiv="X-UA-Compatible" content="ie=edge">
    <title>{module_name}</title>
    <style>
{minireset}

{style}
    </style>
  </head>
  <body>
    <section class="module">
      <dl class="items">
"##,
        pkg_name = env!("CARGO_PKG_NAME"),
        pkg_version = env!("CARGO_PKG_VERSION"),
        module_name = "", // TODO: module name
        minireset = include_str!("./minireset.min.css").trim(),
        style = include_str!("./style.css").trim(),
    )?;

    for item in &module.items {
        let (label, item) = match item {
            core::Item::Alias(alias) => compile_alias(&context, writer, alias, report)?,
            core::Item::Struct(struct_ty) => {
                compile_struct_ty(&context, writer, struct_ty, report)?
            }
        };

        context.items.insert(label, item);
    }

    write!(
        writer,
        r##"      </dl>
    </section>
  </body>
</html>
"##
    )?;

    Ok(())
}

struct ModuleContext {
    _file_id: FileId,
    items: HashMap<core::Label, Item>,
}

struct Item {
    id: String,
}

fn compile_alias(
    context: &ModuleContext,
    writer: &mut impl Write,
    alias: &core::Alias,
    report: &mut dyn FnMut(Diagnostic),
) -> io::Result<(core::Label, Item)> {
    let id = format!("items[{}]", alias.name);

    write!(
        writer,
        r##"        <dt id="{id}" class="item alias">
          <a href="#{id}">{name}</a>
        </dt>
        <dd class="item alias">
"##,
        id = id,
        name = alias.name
    )?;

    if !alias.doc.is_empty() {
        writeln!(writer, r##"          <section class="doc">"##)?;
        compile_doc_lines(writer, "            ", &alias.doc)?;
        writeln!(writer, r##"          </section>"##)?;
    }

    let term = compile_term(context, &alias.term, report);

    write!(
        writer,
        r##"          <section class="term">
            {}
          </section>
        </dd>
"##,
        term
    )?;

    Ok((alias.name.clone(), Item { id }))
}

fn compile_struct_ty(
    context: &ModuleContext,
    writer: &mut impl Write,
    struct_ty: &core::StructType,
    report: &mut dyn FnMut(Diagnostic),
) -> io::Result<(core::Label, Item)> {
    let id = format!("items[{}]", struct_ty.name);

    write!(
        writer,
        r##"        <dt id="{id}" class="item struct">
          struct <a href="#{id}">{name}</a>
        </dt>
        <dd class="item struct">
"##,
        id = id,
        name = struct_ty.name
    )?;

    if !struct_ty.doc.is_empty() {
        writeln!(writer, r##"          <section class="doc">"##)?;
        compile_doc_lines(writer, "            ", &struct_ty.doc)?;
        writeln!(writer, r##"          </section>"##)?;
    }

    if !struct_ty.fields.is_empty() {
        writeln!(writer, r##"          <dl class="fields">"##)?;
        for field in &struct_ty.fields {
            let field_id = format!("{}.fields[{}]", id, field.name);
            let ty = compile_term(context, &field.term, report);

            write!(
                writer,
                r##"            <dt id="{id}" class="field">
              <a href="#{id}">{name}</a> : {ty}
            </dt>
            <dd class="field">
              <section class="doc">
"##,
                id = field_id,
                name = field.name,
                ty = ty,
            )?;
            compile_doc_lines(writer, "                ", &field.doc)?;
            write!(
                writer,
                r##"              </section>
            </dd>
"##
            )?;
        }
        writeln!(writer, r##"          </dl>"##)?;
    }

    writeln!(writer, r##"        </dd>"##)?;

    Ok((struct_ty.name.clone(), Item { id }))
}

fn compile_term<'term>(
    context: &ModuleContext,
    term: &'term core::Term,
    report: &mut dyn FnMut(Diagnostic),
) -> Cow<'term, str> {
    match term {
        // TODO: Link to specific docs
        core::Term::Item(_, name) => {
            let id = match context.items.get(name) {
                Some(item) => item.id.as_str(),
                None => "",
            };

            format!(r##"<var><a href="#{}">{}</a></var>"##, id, name).into()
        }
        core::Term::Ann(term, ty) => {
            let term = compile_term(context, term, report);
            let ty = compile_term(context, ty, report);

            format!("{} : {}", term, ty).into()
        }
        // TODO: Link to global docs
        core::Term::Kind(_) => r##"<var><a href="#">Kind</a></var>"##.into(),
        core::Term::Type(_) => r##"<var><a href="#">Type</a></var>"##.into(),
        core::Term::U8Type(_) => r##"<var><a href="#">U8</a></var>"##.into(),
        core::Term::U16LeType(_) => r##"<var><a href="#">U16Le</a></var>"##.into(),
        core::Term::U16BeType(_) => r##"<var><a href="#">U16Be</a></var>"##.into(),
        core::Term::U32LeType(_) => r##"<var><a href="#">U32Le</a></var>"##.into(),
        core::Term::U32BeType(_) => r##"<var><a href="#">U32Be</a></var>"##.into(),
        core::Term::U64LeType(_) => r##"<var><a href="#">U64Le</a></var>"##.into(),
        core::Term::U64BeType(_) => r##"<var><a href="#">U64Be</a></var>"##.into(),
        core::Term::S8Type(_) => r##"<var><a href="#">S8</a></var>"##.into(),
        core::Term::S16LeType(_) => r##"<var><a href="#">S16Le</a></var>"##.into(),
        core::Term::S16BeType(_) => r##"<var><a href="#">S16Be</a></var>"##.into(),
        core::Term::S32LeType(_) => r##"<var><a href="#">S32Le</a></var>"##.into(),
        core::Term::S32BeType(_) => r##"<var><a href="#">S32Be</a></var>"##.into(),
        core::Term::S64LeType(_) => r##"<var><a href="#">S64Le</a></var>"##.into(),
        core::Term::S64BeType(_) => r##"<var><a href="#">S64Be</a></var>"##.into(),
        core::Term::F32LeType(_) => r##"<var><a href="#">F32Le</a></var>"##.into(),
        core::Term::F32BeType(_) => r##"<var><a href="#">F32Be</a></var>"##.into(),
        core::Term::F64LeType(_) => r##"<var><a href="#">F64Le</a></var>"##.into(),
        core::Term::F64BeType(_) => r##"<var><a href="#">F64Be</a></var>"##.into(),
        core::Term::BoolType(_) => r##"<var><a href="#">Bool</a></var>"##.into(), // NOTE: Invalid if in struct
        core::Term::IntType(_) => r##"<var><a href="#">Int</a></var>"##.into(), // NOTE: Invalid if in struct
        core::Term::F32Type(_) => r##"<var><a href="#">F32</a></var>"##.into(), // NOTE: Invalid if in struct
        core::Term::F64Type(_) => r##"<var><a href="#">F64</a></var>"##.into(), // NOTE: Invalid if in struct
        core::Term::BoolConst(_, true) => r##"<var><a href="#">true</a></var>"##.into(), // TODO: Invalid if in type
        core::Term::BoolConst(_, false) => r##"<var><a href="#">false</a></var>"##.into(), // TODO: Invalid if in type
        core::Term::F32Const(_, value) => format!("{}", value).into(), // TODO: Invalid if in type
        core::Term::F64Const(_, value) => format!("{}", value).into(), // TODO: Invalid if in type
        core::Term::IntConst(_, value) => format!("{}", value).into(), // TODO: Invalid if in type
        core::Term::Error(_) => r##"<strong>(invalid data description)</strong>"##.into(),
    }
}

fn compile_doc_lines(
    writer: &mut impl Write,
    prefix: &str,
    doc_lines: &[String],
) -> io::Result<()> {
    // TODO: parse markdown

    for doc_line in doc_lines.iter() {
        let doc_line = match doc_line {
            line if line.starts_with(" ") => &line[" ".len()..],
            line => &line[..],
        };
        writeln!(writer, "{}{}", prefix, doc_line)?;
    }

    Ok(())
}
