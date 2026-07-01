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
    let mut code: &str = &msg.content;

    // check if message is markdown code block
    if &code[0..3] == "```" && &code[code.len() - 3..] == "```" {
        // strip first line
        code = code.split_once('\n').unwrap().1;
        // strip last three characters
        code = &code[0..code.len() - 3];
    }

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

fn compile_typst(code: &str) -> Result<Vec<u8>, Error> {
    let template = typst_as_lib::TypstEngine::builder()
        .main_file(code)
        .fonts(typst_assets::fonts())
        .build();

    let doc: typst_layout::PagedDocument = template.compile().output?;

    let page = doc.pages().first().ok_or("typst produced no pages")?;
    let ppi = 300.0;
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
                poise::builtins::register_in_guild(
                    ctx,
                    &framework.options().commands,
                    serenity::GuildId::new(1208241314110513162),
                )
                .await?;
                poise::builtins::register_globally(ctx, &Vec::<poise::Command<Data, Error>>::new())
                    .await?;
                Ok(Data {})
            })
        })
        .build();

    let client = serenity::ClientBuilder::new(token, intents)
        .framework(framework)
        .await;
    client.unwrap().start().await.unwrap();
}
