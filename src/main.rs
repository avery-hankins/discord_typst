use poise::serenity_prelude as serenity;
use std::io::Write;
use std::process::{Command, Stdio};

struct Data {} // User data, stored and accessible in command invocations
type Error = Box<dyn std::error::Error + Send + Sync>;
type Context<'a> = poise::Context<'a, Data, Error>;

const PAGE_FRONTMATTER: &str = "#set page(
  width: auto,
  height: auto,
  margin: 0.3cm,
)";

// TODO use typst rust library

/// Renders a user's typst markup
#[poise::command(slash_command)]
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
    // TODO verify typst exists on sys (only on err)?

    let mut compile_child = Command::new("typst")
        .arg("compile")
        .arg("-f")
        .arg("png")
        .arg("--ppi")
        .arg("300")
        .arg("-")
        .arg("-")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    compile_child
        .stdin
        .take()
        .unwrap()
        .write_all(code.as_bytes())?;

    let output = compile_child.wait_with_output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("typst compile failed:\n{stderr}").into());
    }

    Ok(output.stdout)
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
