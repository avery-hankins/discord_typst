use poise::serenity_prelude as serenity;

struct Data {} // User data, stored and accessible in command invocations
type Error = Box<dyn std::error::Error + Send + Sync>;
type Context<'a> = poise::Context<'a, Data, Error>;

const PAGE_FRONTMATTER: &str = "#set page(
  width: auto,
  height: auto,
  margin: 0.3cm,
)";

/// Renders a user's typst markup
#[poise::command(slash_command, rename = "rendertypst")]
async fn typst_slash(
    ctx: Context<'_>,
    #[description = "Typst code"] code: String,
) -> Result<(), Error> {
    render_and_send(ctx, &code).await
}

/// Renders a message's typst markup
#[poise::command(context_menu_command = "Render Typst")]
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

    render_and_send(ctx, code).await
}

async fn render_and_send(ctx: Context<'_>, code: &str) -> Result<(), Error> {
    let code = format!("{PAGE_FRONTMATTER}\n{code}");

    let image_bytes = compile_typst(&code)?;
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

fn compile_typst(code: &str) -> Result<Vec<u8>, Error> {
    let template = typst_as_lib::TypstEngine::builder()
        .main_file(code)
        .fonts(typst_assets::fonts())
        .build();

    let doc: typst_layout::PagedDocument = template.compile().output?;

    let page = doc.pages().first().ok_or("typst produced no pages")?;
    let ppi = choose_ppi(page);
    let pixel_per_pt = typst_utils::Scalar::new(ppi / 72.0);
    let render_options = typst_render::RenderOptions {
        pixel_per_pt,
        render_bleed: false,
    };
    let pixmap = typst_render::render(page, &render_options);

    let png = pixmap
        .encode_png()
        .map_err(|e| format!("png encode failed: {e}"))?;

    Ok(png)
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().expect("Failed to read .env file");
    let token = std::env::var("DISCORD_TOKEN").expect("missing DISCORD_TOKEN");
    let intents =
        serenity::GatewayIntents::non_privileged() | serenity::GatewayIntents::MESSAGE_CONTENT;

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

    let client = serenity::ClientBuilder::new(token, intents)
        .framework(framework)
        .await;
    client.unwrap().start().await.unwrap();
}
