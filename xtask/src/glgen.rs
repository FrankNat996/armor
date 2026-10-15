//! `cargo xtask gl-gen [--check]`: the OpenGL stubs and the host call table
//! (ADR-0041), generated from Khronos' `gl.xml` (`third_party/khronos/`).
//!
//! It writes:
//! - `crates/armor-linux/src/host/gl_table.rs`: for each OpenGL function, its
//!   id (FNV-1a of its name, stable across registry versions), its name and
//!   its signature, and one typed call per distinct signature;
//! - `guest/libgl/gl_stubs.c`: the x86-64 `libGL.so.1` stubs, one per
//!   function, which pass their arguments to armor (syscall 0x410000) when
//!   armor has the host's OpenGL, and otherwise call the guest's own libGL.
//!
//! `--check` fails if the files differ from what the registry gives.
//!
//! The functions are those of the desktop OpenGL versions (`api="gl"`, all
//! of them, removed ones included: the compatibility profile) and of the
//! extensions supported by desktop OpenGL. A function is left out, and
//! listed, when a parameter or its result is a callback, or when it returns
//! a pointer other than a string (memory the driver maps: a later step).

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

/// The class of a parameter or result as it crosses into the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Class {
    /// An integer or a pointer: 8 bytes, passed as `u64`.
    Int,
    /// `float`.
    F32,
    /// `double`.
    F64,
    /// `void` (results only).
    Void,
    /// A NUL-terminated string the host returns (copied into guest memory).
    Str,
}

impl Class {
    fn code(self) -> char {
        match self {
            Class::Int => 'I',
            Class::F32 => 'F',
            Class::F64 => 'D',
            Class::Void => 'V',
            Class::Str => 'S',
        }
    }
}

/// One parameter: its C declaration (`const GLfloat *v`), name and class.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Param {
    declaration: String,
    name: String,
    class: Class,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Command {
    name: String,
    /// The result's C type (`void`, `GLenum`, `const GLubyte *`).
    result: String,
    result_class: Class,
    params: Vec<Param>,
}

/// The functions whose result is a string to copy into guest memory.
const STRING_RESULTS: &[&str] = &["glGetString", "glGetStringi"];

/// The number of argument slots of a host call.
const SLOTS: usize = 16;

/// FNV-1a, 64 bits: a function's id from its name.
pub fn function_id(name: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in name.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// The text between `open` and `close` after `from`, and where it ends.
fn between<'a>(text: &'a str, open: &str, close: &str, from: usize) -> Option<(&'a str, usize)> {
    let start = text[from..].find(open)? + from + open.len();
    let end = text[start..].find(close)? + start;
    Some((&text[start..end], end + close.len()))
}

/// `text` without its XML tags, entities decoded, spaces collapsed.
fn plain(text: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    let out = out
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&");
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The value of attribute `name` in a start tag.
fn attribute<'a>(tag: &'a str, name: &str) -> Option<&'a str> {
    let key = format!(" {name}=\"");
    let start = tag.find(&key)? + key.len();
    let end = tag[start..].find('"')? + start;
    Some(&tag[start..end])
}

/// The class of a C type given as text (`const GLfloat *`, `GLenum`), or
/// `None` for a callback or an unknown type.
fn class_of(text: &str) -> Option<Class> {
    if text.contains('*') {
        return Some(Class::Int);
    }
    let base = text.replace("const", "");
    let base = base.trim();
    match base {
        "GLfloat" | "GLclampf" => Some(Class::F32),
        "GLdouble" | "GLclampd" => Some(Class::F64),
        "void" => Some(Class::Void),
        "GLenum"
        | "GLboolean"
        | "GLbitfield"
        | "GLbyte"
        | "GLshort"
        | "GLint"
        | "GLsizei"
        | "GLubyte"
        | "GLushort"
        | "GLuint"
        | "GLint64"
        | "GLuint64"
        | "GLint64EXT"
        | "GLuint64EXT"
        | "GLintptr"
        | "GLsizeiptr"
        | "GLintptrARB"
        | "GLsizeiptrARB"
        | "GLfixed"
        | "GLclampx"
        | "GLhalf"
        | "GLhalfNV"
        | "GLhalfARB"
        | "GLhandleARB"
        | "GLsync"
        | "GLeglImageOES"
        | "GLeglClientBufferEXT"
        | "GLvdpauSurfaceNV" => Some(Class::Int),
        _ => None,
    }
}

/// Parses the `<command>` blocks of a registry.
fn commands(xml: &str) -> BTreeMap<String, Result<Command, String>> {
    let mut found = BTreeMap::new();
    let mut at = 0;
    while let Some((block, end)) = between(xml, "<command>", "</command>", at) {
        at = end;
        let Some((proto, _)) = between(block, "<proto", "</proto>", 0) else {
            continue;
        };
        let proto = &proto[proto.find('>').map_or(0, |i| i + 1)..];
        let Some((name, _)) = between(proto, "<name>", "</name>", 0) else {
            continue;
        };
        let name = name.to_string();
        let result = plain(&proto[..proto.find("<name>").unwrap_or(0)]);
        let mut params = Vec::new();
        let mut problem = None;
        let mut p = 0;
        while let Some((param, end)) = between(block, "<param", "</param>", p) {
            p = end;
            let param = &param[param.find('>').map_or(0, |i| i + 1)..];
            let declaration = plain(param);
            let Some((param_name, _)) = between(param, "<name>", "</name>", 0) else {
                continue;
            };
            let type_text = plain(&param[..param.find("<name>").unwrap_or(0)]);
            match class_of(&type_text) {
                Some(class) if class != Class::Void => params.push(Param {
                    declaration,
                    name: param_name.to_string(),
                    class,
                }),
                _ => problem = Some(format!("parameter type `{type_text}`")),
            }
        }
        let result_class = if STRING_RESULTS.contains(&name.as_str()) {
            Some(Class::Str)
        } else if result.contains('*') {
            None
        } else {
            class_of(&result)
        };
        let command = match (problem, result_class) {
            (Some(problem), _) => Err(problem),
            (None, None) => Err(format!("result type `{result}`")),
            (None, Some(_)) if params.len() > SLOTS => Err("more than 16 parameters".to_string()),
            (None, Some(result_class)) => Ok(Command {
                name: name.clone(),
                result,
                result_class,
                params,
            }),
        };
        found.insert(name, command);
    }
    found
}

/// The names of the commands desktop OpenGL requires: every `api="gl"`
/// version and every extension that supports `gl`.
fn desktop_commands(xml: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let collect = |block: &str, names: &mut BTreeSet<String>| {
        let mut at = 0;
        while let Some(start) = block[at..].find("<require").map(|i| i + at) {
            let tag_end = block[start..].find('>').map_or(block.len(), |i| i + start);
            let tag = &block[start..tag_end];
            let end = if tag.ends_with('/') {
                tag_end
            } else {
                block[start..]
                    .find("</require>")
                    .map_or(block.len(), |i| i + start)
            };
            at = end.max(start + 1);
            if attribute(tag, "api").is_some_and(|api| api != "gl") {
                continue;
            }
            let mut c = start;
            while let Some((tag, next)) = between(&block[..end], "<command ", "/>", c) {
                c = next;
                if let Some(name) = attribute(&format!(" {tag}"), "name") {
                    names.insert(name.to_string());
                }
            }
        }
    };
    let mut at = 0;
    while let Some(start) = xml[at..].find("<feature ").map(|i| i + at) {
        let end = xml[start..]
            .find("</feature>")
            .map_or(xml.len(), |i| i + start);
        let tag = &xml[start..xml[start..].find('>').map_or(end, |i| i + start)];
        if attribute(tag, "api") == Some("gl") {
            collect(&xml[start..end], &mut names);
        }
        at = end;
    }
    at = 0;
    while let Some(start) = xml[at..].find("<extension ").map(|i| i + at) {
        let tag_end = xml[start..].find('>').map_or(xml.len(), |i| i + start);
        let tag = &xml[start..tag_end];
        let end = if tag.ends_with('/') {
            tag_end
        } else {
            xml[start..]
                .find("</extension>")
                .map_or(xml.len(), |i| i + start)
        };
        if attribute(tag, "supported").is_some_and(|s| s.split('|').any(|api| api == "gl")) {
            collect(&xml[start..end], &mut names);
        }
        at = end.max(start + 1);
    }
    names
}

/// The functions to generate, sorted by name, and those left out with why.
fn selected(xml: &str) -> (Vec<Command>, Vec<(String, String)>) {
    let all = commands(xml);
    let mut chosen = Vec::new();
    let mut left_out = Vec::new();
    for name in desktop_commands(xml) {
        match all.get(&name) {
            Some(Ok(command)) => chosen.push(command.clone()),
            Some(Err(why)) => left_out.push((name, why.clone())),
            None => left_out.push((name, "not defined".to_string())),
        }
    }
    (chosen, left_out)
}

/// A signature's key: the parameters' classes, `>`, the result's.
fn signature_key(command: &Command) -> String {
    let mut key: String = command.params.iter().map(|p| p.class.code()).collect();
    key.push('>');
    key.push(command.result_class.code());
    key
}

const HEADER_RUST: &str = "// Generated by `cargo xtask gl-gen` from third_party/khronos/gl.xml (ADR-0041).\n// Do not edit: change xtask/src/glgen.rs and run it again.\n";

/// The Rust table for armor's host side.
fn rust_table(commands: &[Command]) -> String {
    let mut signatures: Vec<String> = commands.iter().map(signature_key).collect();
    signatures.sort();
    signatures.dedup();
    let mut by_id: Vec<(u64, &Command)> =
        commands.iter().map(|c| (function_id(&c.name), c)).collect();
    by_id.sort_by_key(|(id, _)| *id);
    let mut out = String::from(HEADER_RUST);
    out.push_str("\nuse std::ffi::CStr;\n\n");
    out.push_str("/// The OpenGL functions: id (FNV-1a of the name), name, signature.\n");
    out.push_str(&format!(
        "pub(super) static GL_FUNCTIONS: [(u64, &CStr, u16); {}] = [\n",
        by_id.len()
    ));
    for (id, command) in &by_id {
        let signature = signatures
            .binary_search(&signature_key(command))
            .expect("listed");
        out.push_str(&format!(
            "    ({id:#018x}, c\"{}\", {signature}),\n",
            command.name
        ));
    }
    out.push_str("];\n\n");
    out.push_str("/// Calls `address`, a host function of signature `signature` (an index\n/// into the table's signatures), with the arguments in `s`; the result goes\n/// into `s[0]`.\n///\n/// # Safety\n/// `address` must be a host function of exactly that signature.\n");
    out.push_str("#[allow(clippy::too_many_lines, clippy::cast_possible_truncation)]\n");
    out.push_str("pub(super) unsafe fn call(signature: u16, address: usize, s: &mut [u64; 16]) {\n    match signature {\n");
    for (index, key) in signatures.iter().enumerate() {
        let (params, result) = key.split_once('>').expect("key");
        let rust = |c: char| match c {
            'I' | 'S' => "u64",
            'F' => "f32",
            'D' => "f64",
            _ => "()",
        };
        let types: Vec<&str> = params.chars().map(rust).collect();
        let args: Vec<String> = params
            .chars()
            .enumerate()
            .map(|(i, c)| match c {
                'F' => format!("f32::from_bits(s[{i}] as u32)"),
                'D' => format!("f64::from_bits(s[{i}])"),
                _ => format!("s[{i}]"),
            })
            .collect();
        let result_char = result.chars().next().expect("result");
        let ret = if result_char == 'V' {
            String::new()
        } else {
            format!(" -> {}", rust(result_char))
        };
        out.push_str(&format!("        {index} => {{\n"));
        out.push_str(&format!(
            "            // SAFETY: the caller guarantees the signature.\n            let f: unsafe extern \"C\" fn({}){ret} = unsafe {{ std::mem::transmute(address) }};\n",
            types.join(", ")
        ));
        let call = format!("unsafe {{ f({}) }}", args.join(", "));
        out.push_str("            // SAFETY: as above.\n");
        match result_char {
            'V' => out.push_str(&format!("            {call};\n")),
            'F' => out.push_str(&format!(
                "            s[0] = u64::from({call}.to_bits());\n"
            )),
            'D' => out.push_str(&format!("            s[0] = {call}.to_bits();\n")),
            _ => out.push_str(&format!("            s[0] = {call};\n")),
        }
        out.push_str("        }\n");
    }
    out.push_str(
        "        _ => unreachable!(\"signature {signature} is not in the table\"),\n    }\n}\n",
    );
    out
}

/// The C type definitions the stubs use (the GL types of `gl.xml`'s
/// `<types>`, for Linux x86-64).
const C_TYPES: &str = "#include <stddef.h>
#include <stdint.h>
typedef unsigned int GLenum;
typedef unsigned char GLboolean;
typedef unsigned int GLbitfield;
typedef void GLvoid;
typedef int8_t GLbyte;
typedef uint8_t GLubyte;
typedef int16_t GLshort;
typedef uint16_t GLushort;
typedef int GLint;
typedef unsigned int GLuint;
typedef int32_t GLclampx;
typedef int GLsizei;
typedef float GLfloat;
typedef float GLclampf;
typedef double GLdouble;
typedef double GLclampd;
typedef void *GLeglClientBufferEXT;
typedef void *GLeglImageOES;
typedef char GLchar;
typedef char GLcharARB;
typedef unsigned int GLhandleARB;
typedef uint16_t GLhalf;
typedef uint16_t GLhalfARB;
typedef int32_t GLfixed;
typedef intptr_t GLintptr;
typedef intptr_t GLintptrARB;
typedef intptr_t GLsizeiptr;
typedef intptr_t GLsizeiptrARB;
typedef int64_t GLint64;
typedef int64_t GLint64EXT;
typedef uint64_t GLuint64;
typedef uint64_t GLuint64EXT;
typedef struct __GLsync *GLsync;
struct _cl_context;
struct _cl_event;
typedef uint16_t GLhalfNV;
typedef intptr_t GLvdpauSurfaceNV;
";

/// The guest stubs.
fn c_stubs(commands: &[Command]) -> String {
    let mut out = String::from(
        "/* Generated by `cargo xtask gl-gen` from third_party/khronos/gl.xml (ADR-0041).\n   Do not edit: change xtask/src/glgen.rs and run it again. */\n",
    );
    out.push_str(C_TYPES);
    out.push_str("#include \"armor_gl.h\"\n\n");
    for command in commands {
        let params: Vec<&str> = command
            .params
            .iter()
            .map(|p| p.declaration.as_str())
            .collect();
        let params = if params.is_empty() {
            "void".to_string()
        } else {
            params.join(", ")
        };
        let names: Vec<&str> = command.params.iter().map(|p| p.name.as_str()).collect();
        let id = function_id(&command.name);
        let result = &command.result;
        out.push_str(&format!(
            "ARMOR_EXPORT {result} {}({params}) {{\n",
            command.name
        ));
        out.push_str("    if (armor_gl_on_host()) {\n        uint64_t armor_slots[16] = {0};\n");
        for (i, param) in command.params.iter().enumerate() {
            match param.class {
                Class::F32 => out.push_str(&format!(
                    "        {{ union {{ float f; uint32_t u; }} armor_value = {{ .f = {} }}; armor_slots[{i}] = armor_value.u; }}\n",
                    param.name
                )),
                Class::F64 => out.push_str(&format!(
                    "        {{ union {{ double d; uint64_t u; }} armor_value = {{ .d = {} }}; armor_slots[{i}] = armor_value.u; }}\n",
                    param.name
                )),
                _ if param.declaration.contains('*') => {
                    out.push_str(&format!("        armor_slots[{i}] = (uint64_t)(uintptr_t){};\n", param.name))
                }
                _ => out.push_str(&format!("        armor_slots[{i}] = (uint64_t){};\n", param.name)),
            }
        }
        out.push_str(&format!(
            "        armor_gl_call({id:#x}ULL, armor_slots);\n"
        ));
        match command.result_class {
            Class::Void => {}
            Class::F32 => out.push_str(
                "        { union { float f; uint32_t u; } armor_value = { .u = (uint32_t)armor_slots[0] }; return armor_value.f; }\n",
            ),
            Class::F64 => out.push_str(
                "        { union { double d; uint64_t u; } armor_value = { .u = armor_slots[0] }; return armor_value.d; }\n",
            ),
            _ if result.contains('*') || result.contains("GLsync") => {
                out.push_str(&format!("        return ({result})(uintptr_t)armor_slots[0];\n"))
            }
            _ => out.push_str(&format!("        return ({result})armor_slots[0];\n")),
        }
        if command.result_class == Class::Void {
            out.push_str("        return;\n");
        }
        out.push_str("    }\n");
        out.push_str(&format!(
            "    {{\n        static {result} (*armor_guest)({params});\n        if (!armor_guest) armor_guest = armor_gl_guest(\"{}\");\n        {}armor_guest({});\n    }}\n}}\n\n",
            command.name,
            if command.result_class == Class::Void { "" } else { "return " },
            names.join(", ")
        ));
    }
    out
}

/// `text` as `cargo fmt` writes it (edition 2024), so that the generated
/// table passes `cargo fmt --check`.
fn rustfmt(text: &str) -> Result<String, String> {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let mut child = Command::new("rustfmt")
        .args(["--edition", "2024", "--emit", "stdout"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("rustfmt: {e}"))?;
    child
        .stdin
        .take()
        .expect("piped")
        .write_all(text.as_bytes())
        .map_err(|e| format!("rustfmt: {e}"))?;
    let output = child
        .wait_with_output()
        .map_err(|e| format!("rustfmt: {e}"))?;
    if !output.status.success() {
        return Err(format!("rustfmt failed: {}", output.status));
    }
    String::from_utf8(output.stdout).map_err(|e| format!("rustfmt: {e}"))
}

/// Generates (or with `check`, verifies) the table and the stubs.
pub fn run(root: &Path, check: bool) -> Result<(), String> {
    let xml_path = root.join("third_party/khronos/gl.xml");
    let xml = fs::read_to_string(&xml_path).map_err(|e| format!("{}: {e}", xml_path.display()))?;
    let (commands, left_out) = selected(&xml);
    let mut ids = BTreeMap::new();
    for command in &commands {
        if let Some(other) = ids.insert(function_id(&command.name), &command.name) {
            return Err(format!("id collision: {other} and {}", command.name));
        }
    }
    let outputs = [
        (
            root.join("crates/armor-linux/src/host/gl_table.rs"),
            rustfmt(&rust_table(&commands))?,
        ),
        (root.join("guest/libgl/gl_stubs.c"), c_stubs(&commands)),
    ];
    let mut stale = Vec::new();
    for (path, text) in &outputs {
        let current = fs::read_to_string(path).unwrap_or_default();
        if current != *text {
            if check {
                stale.push(path.display().to_string());
            } else {
                if let Some(dir) = path.parent() {
                    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                }
                fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))?;
            }
        }
    }
    let signatures: BTreeSet<String> = commands.iter().map(signature_key).collect();
    println!(
        "{} OpenGL functions, {} signatures; {} left out",
        commands.len(),
        signatures.len(),
        left_out.len()
    );
    for (name, why) in &left_out {
        println!("  left out: {name} ({why})");
    }
    if stale.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "out of date (run `cargo xtask gl-gen`): {}",
            stale.join(", ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<registry>
    <commands namespace="GL">
        <command>
            <proto>void <name>glClearColor</name></proto>
            <param><ptype>GLfloat</ptype> <name>red</name></param>
            <param><ptype>GLdouble</ptype> <name>depth</name></param>
        </command>
        <command>
            <proto>const <ptype>GLubyte</ptype> *<name>glGetString</name></proto>
            <param group="StringName"><ptype>GLenum</ptype> <name>name</name></param>
        </command>
        <command>
            <proto>void *<name>glMapBuffer</name></proto>
            <param><ptype>GLenum</ptype> <name>target</name></param>
        </command>
        <command>
            <proto>void <name>glDebugMessageCallback</name></proto>
            <param><ptype>GLDEBUGPROC</ptype> <name>callback</name></param>
        </command>
        <command>
            <proto><ptype>GLboolean</ptype> <name>glIsTexture</name></proto>
            <param>const void *<name>pointer</name></param>
        </command>
        <command>
            <proto>void <name>glOnlyES</name></proto>
        </command>
    </commands>
    <feature api="gl" name="GL_VERSION_1_0" number="1.0">
        <require>
            <command name="glClearColor"/>
            <command name="glGetString"/>
            <command name="glMapBuffer"/>
        </require>
    </feature>
    <feature api="gles2" name="GL_ES_VERSION_2_0" number="2.0">
        <require><command name="glOnlyES"/></require>
    </feature>
    <extensions>
        <extension name="GL_KHR_debug" supported="gl|glcore|gles2">
            <require><command name="glDebugMessageCallback"/></require>
            <require api="gles2"><command name="glOnlyES"/></require>
        </extension>
        <extension name="GL_X_texture" supported="gl">
            <require><command name="glIsTexture"/></require>
        </extension>
    </extensions>
</registry>"#;

    #[test]
    fn commands_are_selected_and_classified() {
        let (chosen, left_out) = selected(SAMPLE);
        let names: Vec<&str> = chosen.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["glClearColor", "glGetString", "glIsTexture"]);
        assert_eq!(signature_key(&chosen[0]), "FD>V");
        assert_eq!(signature_key(&chosen[1]), "I>S");
        assert_eq!(chosen[1].result, "const GLubyte *");
        assert_eq!(signature_key(&chosen[2]), "I>I");
        assert_eq!(chosen[2].params[0].declaration, "const void *pointer");
        let left: Vec<&str> = left_out.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(left, ["glDebugMessageCallback", "glMapBuffer"]);
    }

    #[test]
    fn ids_are_fnv1a() {
        // FNV-1a 64 of "a" (the published test vector).
        assert_eq!(function_id("a"), 0xaf63_dc4c_8601_ec8c);
        assert_ne!(function_id("glClear"), function_id("glClearColor"));
    }

    #[test]
    fn stubs_and_table_are_generated() {
        let (chosen, _) = selected(SAMPLE);
        let c = c_stubs(&chosen);
        assert!(c.contains("ARMOR_EXPORT void glClearColor(GLfloat red, GLdouble depth) {"));
        assert!(c.contains("armor_value = { .f = red }; armor_slots[0] = armor_value.u;"));
        assert!(c.contains("return (const GLubyte *)(uintptr_t)armor_slots[0];"));
        let rust = rust_table(&chosen);
        assert!(rust.contains("c\"glGetString\""));
        assert!(rust.contains("unsafe extern \"C\" fn(f32, f64)"));
    }
}
