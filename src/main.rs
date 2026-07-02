use poise::serenity_prelude as serenity;
use typst::diag::{Severity, SourceDiagnostic};
use typst::syntax::{DiagSpanKind, Source};

struct Data {} // User data, stored and accessible in command invocations
type Error = Box<dyn std::error::Error + Send + Sync>;
type Context<'a> = poise::Context<'a, Data, Error>;

enum CompileError {
    Source(String), // user error
    Internal(Error),
}

const PAGE_FRONTMATTER: &str = "#set page(
  width: auto,
  height: auto,
  margin: 0.3cm,
)";
const FRONTMATTER_LINES: usize = 5;

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
        match compile_typst(&formatted_code) {
            Ok(image_bytes) => {
                send_img_bytes(poise::Context::Application(ctx), image_bytes).await?;
                return Ok(());
            }
            Err(e) => {
                if let CompileError::Internal(_) = e {
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
    match compile_typst(&formatted_code) {
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
/// Small content should stay crisp while large pages scale down.
fn choose_ppi(page: &typst_layout::Page) -> f64 {
    let size = page.frame.size();
    let w_pt = size.x.to_pt();
    let h_pt = size.y.to_pt();

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

fn compile_typst(code: &str) -> Result<Vec<u8>, CompileError> {
    let source = Source::detached(code);
    let template = typst_as_lib::TypstEngine::builder()
        .main_file(source.clone())
        .fonts(typst_assets::fonts())
        .build();

    let doc: typst_layout::PagedDocument = match template.compile().output {
        Ok(doc) => doc,
        Err(typst_as_lib::TypstAsLibError::TypstSource(diags)) => {
            return Err(CompileError::Source(format_diagnostics(&diags, &source)));
        }
        Err(e) => return Err(CompileError::Internal(e.into())),
    };

    let page = match doc.pages().first() {
        Some(page) => page,
        None => {
            return Err(CompileError::Internal("typst produced no pages".into()));
        }
    };
    let ppi = choose_ppi(page);
    let pixel_per_pt = typst_utils::Scalar::new(ppi / 72.0);
    let render_options = typst_render::RenderOptions {
        pixel_per_pt,
        render_bleed: false,
    };
    let pixmap = typst_render::render(page, &render_options);

    match pixmap.encode_png() {
        Ok(bytes) => Ok(bytes),
        Err(e) => Err(CompileError::Internal(e.into())),
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
        CompileError::Source(diags) => ("Typst compile error", diags.as_str()),
        CompileError::Internal(e) => {
            eprintln!("Internal error: {e}");
            (
                "Something went wrong",
                "Internal error while rendering. Please try again later.",
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

#[tokio::main]
async fn main() {
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
