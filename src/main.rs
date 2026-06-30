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
#[poise::command(slash_command, prefix_command)]
async fn typst(
    ctx: Context<'_>,
    #[description = "Typst code"] code: Option<String>,
) -> Result<(), Error> {
    let referenced_message = match ctx {
        poise::Context::Prefix(prefix_ctx) => prefix_ctx.msg.referenced_message.as_deref(),
        poise::Context::Application(_) => None,
    };

    let code = match code {
        Some(code) => code,
        // fall back to the replied-to message's content
        None => referenced_message
            .map(|m| m.content.clone())
            .ok_or("no code provided and no referenced message")?,
    };

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
            commands: vec![typst()],
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
