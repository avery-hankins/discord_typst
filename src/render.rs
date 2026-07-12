use serde::{Deserialize, Serialize};
use std::sync::LazyLock;
use tokio::sync::Semaphore;
use typst::diag::{Severity, SourceDiagnostic};
use typst::foundations::Bytes;
use typst::layout::Abs;
use typst::syntax::{DiagSpanKind, Source};
use typst::text::Font;
use typst::visualize::Color;

/// Embedded fonts, decoded once and reused across every compile.
pub static FONTS: LazyLock<Vec<Font>> = LazyLock::new(|| {
    typst_assets::fonts()
        .flat_map(|bytes| Font::iter(Bytes::new(bytes)))
        .collect()
});

/// Max number of processes that can be run at the same time.
const MAX_PROCESSES: usize = 5;
static COMPILE_SLOTS: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(MAX_PROCESSES));

#[derive(Debug, Serialize, Deserialize)]
pub enum CompileError {
    Source(String),   // User error (bad typst code)
    Internal(String), // Simple error message, can use serde_error crate if needs to have backtrace,
    // and other details.
    Timeout,         // Compile took too long (MAX_COMPILE_SECONDS).
    Crashed(String), // Compile failed for some other reason.
}

pub const PAGE_FRONTMATTER: &str = "#set page(
  width: auto,
  height: auto,
  margin: 0.3cm,
)";
const FRONTMATTER_LINES: usize = 5;
/// Vertical gap (in pt) inserted between pages when stitching a multi-page doc.
const PAGE_GAP_PT: f64 = 8.0;

pub const MAX_COMPILE_SECONDS: u64 = 2 * 60;
const MAX_COMPILE_BYTES: u64 = 1024 * 1024 * 1024; // 1 GB

/// Picks the highest rendering ppi that keeps the output within pixel budgets.
/// Small content stays crisp while large pages scale down.
fn choose_ppi(w_pt: f64, h_pt: f64) -> f64 {
    let target_ppi = 576.0; // ppi for small content
    let min_ppi = 144.0; // ppi for large content
    let max_side = 2400.0; // px, longest edge
    let max_mp = 4.0; // megapixels

    let scale = target_ppi / 72.0; // px per pt at target ppi
    let w_px = w_pt * scale;
    let h_px = h_pt * scale;
    if w_px <= 0.0 || h_px <= 0.0 {
        return target_ppi;
    }

    // Shrink factor to fit under both budgets; <= 1.0 means downscale.
    let side_fit = max_side / w_px.max(h_px);
    let mp_fit = (max_mp * 1e6 / (w_px * h_px)).sqrt();
    let fit = side_fit.min(mp_fit).min(1.0);

    (target_ppi * fit).max(min_ppi)
}

/// Creates a subprocess to run the typst compilation/rendering.
/// This process gets killed when it takes too long or uses too much memory.
pub async fn compile_in_subprocess(code: String) -> Result<Vec<u8>, CompileError> {
    // wait for compile slot
    let _permit = COMPILE_SLOTS
        .acquire()
        .await
        .expect("failed to acquire semaphore");

    let mut compile_handle = procspawn::spawn(code, |code| {
        // Limit address space so a (too) large compile gets aborted.
        let rlimit_res =
            rlimit::setrlimit(rlimit::Resource::AS, MAX_COMPILE_BYTES, MAX_COMPILE_BYTES);
        if let Err(e) = rlimit_res {
            eprintln!("failed to set address space cap: {}", e);
        }

        compile_typst(&code)
    });

    tokio::task::spawn_blocking(move || {
        match compile_handle.join_timeout(std::time::Duration::from_secs(MAX_COMPILE_SECONDS)) {
            Ok(res) => res,
            Err(e) if e.is_timeout() => {
                let _ = compile_handle.kill();
                Err(CompileError::Timeout)
            }
            Err(e) if e.is_panic() || e.is_remote_close() => {
                let _ = compile_handle.kill();
                Err(CompileError::Crashed(e.to_string()))
            }
            Err(e) => {
                let _ = compile_handle.kill();
                Err(CompileError::Internal(e.to_string()))
            }
        }
    })
    .await
    .unwrap_or_else(|join_err| Err(CompileError::Internal(join_err.to_string())))
}

/// Compiles user-supplied Typst markup to a PNG.
///
/// SANDBOX: the engine is built without filesystem or package resolver,
/// so untrusted code cannot read local files, or import `@preview` packages.
/// This runs arbitrary user input, so do NOT add a filesystem/package resolver here,
/// without gating what it can reach.
pub fn compile_typst(code: &str) -> Result<Vec<u8>, CompileError> {
    let source = Source::detached(code);
    let template = typst_as_lib::TypstEngine::builder()
        .main_file(source.clone())
        .fonts(FONTS.iter().cloned())
        .build();

    let doc: typst_layout::PagedDocument = match template.compile().output {
        Ok(doc) => doc,
        Err(typst_as_lib::TypstAsLibError::TypstSource(diags)) => {
            return Err(CompileError::Source(format_diagnostics(&diags, &source)));
        }
        Err(e) => return Err(CompileError::Internal(e.to_string())),
    };

    if doc.pages().is_empty() {
        return Err(CompileError::Internal("typst produced no pages".into()));
    }

    // Budget the ppi against the whole stitched image (widest page by total
    // height, incl. gaps).
    let mut combined_w: f64 = 0.0;
    let mut combined_h: f64 = 0.0;
    for page in doc.pages() {
        let size = page.frame.size();
        combined_w = combined_w.max(size.x.to_pt());
        combined_h += size.y.to_pt();
    }
    combined_h += PAGE_GAP_PT * doc.pages().len().saturating_sub(1) as f64;

    let ppi = choose_ppi(combined_w, combined_h);
    let render_options = typst_render::RenderOptions {
        pixel_per_pt: typst_utils::Scalar::new(ppi / 72.0),
        render_bleed: false,
    };
    // Render every page, stacked vertically with a white gap, into one image.
    let pixmap = typst_render::render_merged(
        &doc,
        &render_options,
        Abs::pt(PAGE_GAP_PT),
        Some(Color::WHITE),
    );

    match pixmap.encode_png() {
        Ok(bytes) => Ok(bytes),
        Err(e) => Err(CompileError::Internal(e.to_string())),
    }
}

/// Takes errors and warnings from Typst compilation process,
/// formats them to be readable for app user.
fn format_diagnostics(diags: &[SourceDiagnostic], source: &Source) -> String {
    let mut out = String::new();
    for (i, d) in diags.iter().enumerate() {
        if i >= 5 {
            if diags.len() == 6 {
                out.push_str("\n… and 1 more error");
            } else {
                out.push_str(&format!("\n… and {} more errors", diags.len() - i));
            }
            break;
        }
        if i > 0 {
            out.push_str("\n\n");
        }

        let severity = match d.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };

        // Line number, if the span resolves to our source
        let line = match d.span.get() {
            DiagSpanKind::Number { id, num, sub_range } if id == source.id() => {
                source.range(num, sub_range)
            }
            DiagSpanKind::Range { id, range } if id == source.id() => Some(range),
            _ => None, // detached or points at another file
        }
        .and_then(|r| source.lines().byte_to_line(r.start))
        .map(|l| l + 1) // byte_to_line is 0-indexed
        .filter(|l| *l > FRONTMATTER_LINES) // error inside frontmatter = our bug, hide the number
        .map(|l| format!(" (line {})", l - FRONTMATTER_LINES));

        out.push_str(&format!(
            "{severity}: {}{}",
            d.message,
            line.unwrap_or_default()
        ));
        for hint in &d.hints {
            out.push_str(&format!("\n  hint: {}", hint.v));
        }
    }
    out
}
