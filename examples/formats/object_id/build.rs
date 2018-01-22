extern crate ddl;

use std::env;
use std::fs::File;
use std::io::prelude::*;

use ddl::syntax::ast::Module;
use ddl::parser::ast::Module as ParseModule;

fn main() {
    let src = {
        let mut src_file = File::open("src/object_id.ddl").unwrap();
        let mut src = String::new();
        src_file.read_to_string(&mut src).unwrap();
        src
    };

    let mut module = Module::from_parse(&ParseModule::from_str(&src).unwrap()).unwrap();
    module.substitute(&Module::prelude());
    ddl::syntax::check::check_module(&module).unwrap();
    let ir = ddl::ir::ast::Module::from(&module);

    let out_dir = env::var("OUT_DIR").unwrap();
    let mut file = File::create(out_dir + "/object_id.rs").unwrap();
    write!(file, "{}", ddl::codegen::LowerModule(&ir)).unwrap();
}
