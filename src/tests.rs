use crate::render::{CompileError, PAGE_FRONTMATTER, compile_typst};

/// Compiles user code wrapped in our frontmatter, like the real handlers do.
fn compile(code: &str) -> Result<Vec<u8>, CompileError> {
    compile_typst(&format!("{PAGE_FRONTMATTER}\n{code}"))
}

#[test]
fn renders_basic_content() {
    assert!(compile("hello *world*").is_ok());
}

#[test]
fn renders_multiple_pages() {
    // Force two pages; both should end up in one (non-empty) PNG.
    let png = compile("first\n#pagebreak()\nsecond").expect("should render");
    assert!(!png.is_empty());
}

// SANDBOX guards: untrusted code must not reach the filesystem or network.
// Each should be rejected as a *user* error (clean diagnostic), never a
// successful render and never an internal error leaked to the user.

#[test]
fn rejects_file_read() {
    match compile(r#"#read("/etc/passwd")"#) {
        Err(CompileError::Source(msg)) => assert!(!msg.is_empty()),
        other => panic!(
            "expected sandbox to reject file read, got {:?}",
            other.is_ok()
        ),
    }
}

#[test]
fn rejects_package_import() {
    match compile(r#"#import "@preview/cetz:0.2.0": *"#) {
        Err(CompileError::Source(msg)) => assert!(!msg.is_empty()),
        other => panic!(
            "expected sandbox to reject package import, got {:?}",
            other.is_ok()
        ),
    }
}
