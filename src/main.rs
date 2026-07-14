mod render;
#[cfg(test)]
mod tests;

use poise::serenity_prelude as serenity;
use std::sync::LazyLock;

use render::{CompileError, FONTS, MAX_COMPILE_SECONDS, PAGE_FRONTMATTER, compile_in_subprocess};

struct Data {} // User data, stored and accessible in command invocations
type Error = Box<dyn std::error::Error + Send + Sync>;
type Context<'a> = poise::Context<'a, Data, Error>;

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
    if msg.content.trim().is_empty() {
        ctx.send(
            poise::CreateReply::default()
                .ephemeral(true)
                .content("No text found in message."),
        )
        .await?;
        return Ok(());
    }

    let code = strip_code_block(msg.content.trim());

    let formatted_code = format!("{PAGE_FRONTMATTER}\n{code}");
    ctx.defer().await?; // compilation may take awhile
    let compile_result = compile_in_subprocess(formatted_code).await;
    match compile_result {
        Ok(image_bytes) => send_img_bytes(ctx, image_bytes).await,
        Err(e) => {
            ctx.send(error_reply(&e)).await?;
            Ok(())
        }
    }
}

/// If the message is a ```markdown code block```, strip all non-content (including language).
fn strip_code_block(content: &str) -> &str {
    content
        .strip_prefix("```")
        .and_then(|s| s.strip_suffix("```"))
        .map(|inner| match inner.split_once('\n') {
            Some((_first_line, rest)) => rest,
            None => inner,
        })
        .unwrap_or(content)
}

/// Takes raw image data and sends as a png to discord.
async fn send_img_bytes(ctx: Context<'_>, image_bytes: Vec<u8>) -> Result<(), Error> {
    let attachment = serenity::CreateAttachment::bytes(image_bytes, "output.png");
    let reply = poise::CreateReply::default().attachment(attachment);
    ctx.send(reply).await?;

    Ok(())
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
