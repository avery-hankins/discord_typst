use poise::serenity_prelude as serenity;
use std::process::Command;

struct Data {} // User data, stored and accessible in command invocations
type Error = Box<dyn std::error::Error + Send + Sync>;
type Context<'a> = poise::Context<'a, Data, Error>;

const PAGE_FRONTMATTER: &str = "#set page(
  width: auto,
  height: auto,
  margin: 0.3cm,
)";

/// Displays a user's account creation date
#[poise::command(slash_command, prefix_command)]
async fn age(
    ctx: Context<'_>,
    #[description = "Selected user"] user: Option<serenity::User>,
) -> Result<(), Error> {
    let u = user.as_ref().unwrap_or_else(|| ctx.author());
    let response = format!("{}'s account was created at {}", u.name, u.created_at());
    ctx.say(response).await?;
    Ok(())
}

/// Renders a user's typst markup
#[poise::command(slash_command, prefix_command)]
async fn typst(
    ctx: Context<'_>,
    #[description = "Typst code"] code: Option<String>,
) -> Result<(), Error> {
    let code = format!("{}\n{}", PAGE_FRONTMATTER, code.unwrap_or_else(|| todo!()));
    let image_path = "/tmp/discord_typst.png";

    let _ = compile_typst(&code, image_path);
    let attachment = serenity::CreateAttachment::path(image_path).await.unwrap();
    let reply = poise::CreateReply::default().attachment(attachment);
    ctx.send(reply).await?;

    Ok(())
}

fn compile_typst(code: &str, image_path: &str) -> Result<(), Error> {
    let code_path = "/tmp/discord_typst.typst";
    std::fs::write(code_path, code)?;

    // TODO verify typst exists on sys (only on err)?

    let cmd_res = Command::new("typst")
        .arg("compile")
        .arg(code_path)
        .arg(image_path)
        .output()
        .expect("failed to execute process");

    dbg!(cmd_res); //TODO remove
    Ok(())
}

#[tokio::main]
async fn main() {
    dotenvy::dotenv().expect("Failed to read .env file");
    let token = std::env::var("DISCORD_TOKEN").expect("missing DISCORD_TOKEN");
    let intents = serenity::GatewayIntents::non_privileged();

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: vec![age(), typst()],
            ..Default::default()
        })
        .setup(|ctx, _ready, framework| {
            Box::pin(async move {
                poise::builtins::register_in_guild(ctx, &framework.options().commands, serenity::GuildId::new(1208241314110513162)).await?;
                Ok(Data {})
            })
        })
        .build();

    let client = serenity::ClientBuilder::new(token, intents)
        .framework(framework)
        .await;
    client.unwrap().start().await.unwrap();
}
