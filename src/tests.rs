use crate::render::{
    CompileError, FRONTMATTER_LINES, PAGE_FRONTMATTER, choose_ppi,
    compile_in_subprocess_with_timeout, compile_typst,
};
use crate::strip_code_block;

procspawn::enable_test_support!();

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

/// Diagnostic line numbers are shifted by FRONTMATTER_LINES; if the
/// frontmatter grows or shrinks without updating the constant, users get
/// wrong line numbers. This pins the two together.
#[test]
fn frontmatter_lines_constant_matches_frontmatter() {
    assert_eq!(PAGE_FRONTMATTER.lines().count(), FRONTMATTER_LINES);
}

/// A slow compile must come back as Timeout, with the subprocess killed,
/// not hang the caller. Uses a short test-only timeout instead of the
/// production MAX_COMPILE_SECONDS.
#[tokio::test]
async fn slow_compile_times_out() {
    // Nested for loops: ~1e10 interpreter iterations that typst can neither
    // memoize away (not a function call) nor reject up front.
    let code =
        format!("{PAGE_FRONTMATTER}\n#for i in range(100000) {{ for j in range(100000) {{ }} }}");
    let res = compile_in_subprocess_with_timeout(code, std::time::Duration::from_secs(5)).await;
    assert!(
        matches!(res, Err(CompileError::Timeout)),
        "got {:?}",
        res.map(|_| "Ok(png)")
    );
}

// choose_ppi: pixel budgets are max_side = 2400px and max_mp = 4.0,
// between target_ppi 576 and floor 144.

#[test]
fn choose_ppi_small_content_gets_target_ppi() {
    // 100pt at 576ppi = 800px per side, well within both budgets.
    assert_eq!(choose_ppi(100.0, 100.0), 576.0);
}

#[test]
fn choose_ppi_huge_content_clamps_to_min() {
    assert_eq!(choose_ppi(10_000.0, 10_000.0), 144.0);
}

#[test]
fn choose_ppi_respects_max_side() {
    // 400pt at 576ppi = 3200px, over the 2400px side budget.
    let ppi = choose_ppi(400.0, 100.0);
    let longest_px = 400.0 * ppi / 72.0;
    assert!((longest_px - 2400.0).abs() < 1e-6, "got {longest_px}px");
}

#[test]
fn choose_ppi_respects_megapixel_budget() {
    // 300pt square at 576ppi = 2400x2400 = 5.76MP, over the 4MP budget
    // (but within the side budget, so this isolates the MP branch).
    let ppi = choose_ppi(300.0, 300.0);
    let side_px = 300.0 * ppi / 72.0;
    assert!(
        (side_px * side_px - 4e6).abs() < 1.0,
        "got {}MP",
        side_px * side_px / 1e6
    );
}

#[test]
fn strip_code_block_removes_fences_and_language() {
    assert_eq!(strip_code_block("```typst\n$x^2$\n```"), "$x^2$\n");
    assert_eq!(strip_code_block("```\n$x^2$\n```"), "$x^2$\n");
}

#[test]
fn strip_code_block_leaves_plain_text_alone() {
    assert_eq!(strip_code_block("hello *world*"), "hello *world*");
    // Unbalanced fence: not a code block, pass through untouched.
    assert_eq!(strip_code_block("```typst\n$x^2$"), "```typst\n$x^2$");
}

#[test]
fn strip_code_block_leaves_intentional_blocks() {
    // When the content itself discusses code blocks, those inner blocks should be preserved.
    let input = "```\nThis code:\n```rust\nlet x: usize = 1;\n```\ndeclares a variable.\n```";
    let expected = "This code:\n```rust\nlet x: usize = 1;\n```\ndeclares a variable.\n";
    assert_eq!(strip_code_block(input), expected);

    // Multiple inner blocks
    let input = "```\nExample 1:\n```python\nprint('hello')\n```\n\nExample 2:\n```js\nconsole.log('hi')\n```\n```";
    let expected =
        "Example 1:\n```python\nprint('hello')\n```\n\nExample 2:\n```js\nconsole.log('hi')\n```\n";
    assert_eq!(strip_code_block(input), expected);

    let input =
        "Example 1:\n```python\nprint('hello')\n```\n\nExample 2:\n```js\nconsole.log('hi')\n```";
    let expected =
        "Example 1:\n```python\nprint('hello')\n```\n\nExample 2:\n```js\nconsole.log('hi')\n```";
    assert_eq!(strip_code_block(input), expected);
}

#[test]
fn strip_code_block_single_line_block_keeps_content() {
    // No newline means no language line to strip; everything inside stays.
    assert_eq!(strip_code_block("```$x^2$```"), "$x^2$");
}

// SANDBOX guards: untrusted code must not reach the filesystem or network.
// Each should be rejected as a *user* error, never a successful render
// and never an internal error leaked to the user.

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
