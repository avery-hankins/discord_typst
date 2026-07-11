use poise::serenity_prelude as serenity;
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
static FONTS: LazyLock<Vec<Font>> = LazyLock::new(|| {
    typst_assets::fonts()
        .flat_map(|bytes| Font::iter(Bytes::new(bytes)))
        .collect()
});

/// Max number of processes that can be run at the same time.
const MAX_PROCESSES: usize = 5;
static COMPILE_SLOTS: LazyLock<Semaphore> = LazyLock::new(|| Semaphore::new(MAX_PROCESSES));

struct Data {} // User data, stored and accessible in command invocations
type Error = Box<dyn std::error::Error + Send + Sync>;
type Context<'a> = poise::Context<'a, Data, Error>;

#[derive(Debug, Serialize, Deserialize)]
enum CompileError {
    Source(String),   // User error (bad typst code)
    Internal(String), // Simple error message, can use serde_error crate if needs to have backtrace,
    // and other details.
    Timeout,         // Compile took too long (MAX_COMPILE_SECONDS).
    Crashed(String), // Compile failed for some other reason.
}

const PAGE_FRONTMATTER: &str = "#set page(
  width: auto,
  height: auto,
  margin: 0.3cm,
)";
const FRONTMATTER_LINES: usize = 5;
/// Vertical gap (in pt) inserted between pages when stitching a multi-page doc.
const PAGE_GAP_PT: f64 = 8.0;

const MAX_COMPILE_SECONDS: u64 = 2 * 60;
const MAX_COMPILE_BYTES: u64 = 1024 * 1024 * 1024; // 1 GB

#[derive(poise::Modal)]
#[name = "Render Typst"]
struct TypstModal {
    #[name = "Typst code"]
    #[paragraph]
    code: String,
}

/// Renders a user's typst markup
#[poise::command(
    slash_command,
    rename = "rendertypst",
    install_context = "Guild | User",
    interaction_context = "Guild | PrivateChannel"
)]
async fn typst_slash(ctx: poise::ApplicationContext<'_, Data, Error>) -> Result<(), Error> {
    let Some(mut modal): Option<TypstModal> = poise::execute_modal(ctx, None, None).await? else {
        return Ok(());
    };

    loop {
        let formatted_code = format!("{PAGE_FRONTMATTER}\n{}", modal.code);

        // Show an ephemeral loading notice so the invoker gets feedback
        // then replace on error/success.
        let notice = ctx
            .send(
                poise::CreateReply::default()
                    .ephemeral(true)
                    .content("Rendering…"),
            )
            .await?;

        let compile_result = compile_in_subprocess(formatted_code).await;
        let _ = notice.delete(poise::Context::Application(ctx)).await;
        match compile_result {
            Ok(image_bytes) => {
                send_img_bytes(poise::Context::Application(ctx), image_bytes).await?;
                return Ok(());
            }
            Err(e) => {
                if matches!(e, CompileError::Internal(_) | CompileError::Crashed(_)) {
                    ctx.send(error_reply(&e)).await?;
                    return Ok(());
                }

                // Unique per invocation so collectors don't cross-talk.
                let retry_id = format!("retry-{}", ctx.interaction.id);

                let reply =
                    error_reply(&e).components(vec![serenity::CreateActionRow::Buttons(vec![
                        serenity::CreateButton::new(&retry_id).label("Edit & retry"),
                    ])]);
                ctx.send(reply).await?;

                // Wait for the button. Fresh interaction we can attach a modal to.
                let press = serenity::ComponentInteractionCollector::new(
                    ctx.serenity_context().shard.clone(),
                )
                .filter(move |i| i.data.custom_id == retry_id)
                .author_id(ctx.author().id)
                .timeout(std::time::Duration::from_secs(600))
                .await;

                let Some(press) = press else {
                    return Ok(()); // user gave up
                };

                // Reopen modal off the BUTTON interaction, prefilled with their code.
                let Some(next) = poise::execute_modal_on_component_interaction(
                    ctx,
                    press,
                    Some(modal), // prefill code from previous attempt
                    None,
                )
                .await?
                else {
                    return Ok(());
                };
                modal = next;
            }
        }
    }
}

/// Renders a message's typst markup
#[poise::command(
    context_menu_command = "Render Typst",
    install_context = "Guild | User",
    interaction_context = "Guild | PrivateChannel"
)]
async fn typst_msg(ctx: Context<'_>, msg: serenity::model::channel::Message) -> Result<(), Error> {
    ctx.defer().await?; // compilation may take awhile
    let content = msg.content.trim();

    // If the message is a ```markdown code block```, strip all non-content (including language).
    let code = content
        .strip_prefix("```")
        .and_then(|s| s.strip_suffix("```"))
        .map(|inner| match inner.split_once('\n') {
            Some((_first_line, rest)) => rest,
            None => inner,
        })
        .unwrap_or(content);

    let formatted_code = format!("{PAGE_FRONTMATTER}\n{code}");
    let compile_result = compile_in_subprocess(formatted_code).await;
    match compile_result {
        Ok(image_bytes) => send_img_bytes(ctx, image_bytes).await,
        Err(e) => {
            ctx.send(error_reply(&e)).await?;
            Ok(())
        }
    }
}

/// Takes raw image data and sends as a png to discord.
async fn send_img_bytes(ctx: Context<'_>, image_bytes: Vec<u8>) -> Result<(), Error> {
    let attachment = serenity::CreateAttachment::bytes(image_bytes, "output.png");
    let reply = poise::CreateReply::default().attachment(attachment);
    ctx.send(reply).await?;

    Ok(())
}

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
async fn compile_in_subprocess(code: String) -> Result<Vec<u8>, CompileError> {
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
fn compile_typst(code: &str) -> Result<Vec<u8>, CompileError> {
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

fn error_reply(err: &CompileError) -> poise::CreateReply {
    let (title, body) = match err {
        CompileError::Source(diags) => ("Typst compile error", diags.clone()),
        CompileError::Internal(e) => {
            eprintln!("Internal error: {e}");
            (
                "Something went wrong",
                "Internal error while rendering. Please try again later.".to_string(),
            )
        }
        CompileError::Timeout => (
            "Timeout error",
            format!(
                "Your code took longer than {} seconds to compile. Please try something simpler.",
                MAX_COMPILE_SECONDS
            ),
        ),
        CompileError::Crashed(e) => {
            eprintln!("Compilation crash: {e}");
            (
                "Your rendering crashed",
                "Something went wrong. Please try again later or try a simpler typst program (this error can be caused when the process runs out of memory).".to_string(),
            )
        }
    };

    // use zero width space to stop code block breakage
    let body = body.replace("```", "`\u{200b}``");

    // only take first 4k characters
    let body: String = body.chars().take(4000).collect();

    poise::CreateReply::default().ephemeral(true).embed(
        serenity::CreateEmbed::new()
            .title(title)
            .color(0xED4245)
            .description(format!("```\n{body}\n```")),
    )
}

fn main() {
    // warm fonts in each subprocess
    LazyLock::force(&FONTS);

    // start point for spawned processes. created in non-async func to avoid multiple tokio runtimes
    // being created.
    procspawn::init();

    let rt = tokio::runtime::Runtime::new().expect("failed to create async runtime");
    rt.block_on(bot_start()); // start the bot
}

async fn bot_start() {
    dotenvy::dotenv().expect("failed to read .env file");
    let token = std::env::var("DISCORD_TOKEN").expect("missing DISCORD_TOKEN");
    let intents = serenity::GatewayIntents::non_privileged();

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: vec![typst_slash(), typst_msg()],
            ..Default::default()
        })
        .setup(|ctx, _ready, framework| {
            Box::pin(async move {
                poise::builtins::register_globally(ctx, &framework.options().commands).await?;
                Ok(Data {})
            })
        })
        .build();

    let mut client = serenity::ClientBuilder::new(token, intents)
        .framework(framework)
        .await
        .expect("failed to build client");

    client.start().await.expect("failed to run client");
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
