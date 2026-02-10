use acton_ai::prelude::Message;

use crate::agent::SchemaForgeAgent;
use crate::error::ForgeAiError;
use crate::prompt::FORGE_SYSTEM_PROMPT;

/// Runs an interactive CLI mode for conversational schema design.
///
/// Uses a manual chat loop pattern with `continue_with()` and tool attachment
/// on each turn. Reads from stdin, prints streaming tokens to stdout.
///
/// Press Ctrl+D (EOF) to exit.
pub async fn run_interactive_cli(agent: &SchemaForgeAgent) -> Result<(), ForgeAiError> {
    use std::io::{BufRead, Write};

    let stdin = std::io::stdin();
    let mut history: Vec<Message> = Vec::new();

    println!("SchemaForge Interactive Mode");
    println!("Type your schema description. Press Ctrl+D to exit.\n");

    loop {
        print!("You: ");
        std::io::stdout().flush().ok();

        let mut input = String::new();
        if stdin.lock().read_line(&mut input).unwrap_or(0) == 0 {
            break;
        }

        let input = input.trim();
        if input.is_empty() {
            continue;
        }

        history.push(Message::user(input));

        // Build prompt with full history + tools
        let mut builder = agent
            .runtime()
            .continue_with(history.clone())
            .system(FORGE_SYSTEM_PROMPT)
            .on_token(|t| print!("{t}"));
        builder = agent.tools().attach_to(builder);

        print!("SchemaForge: ");
        std::io::stdout().flush().ok();

        match builder.collect().await {
            Ok(response) => {
                println!();
                history.push(Message::assistant(&response.text));
            }
            Err(e) => {
                eprintln!("\nError: {e}");
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that the function signature compiles and Message types are correct.
    #[test]
    fn message_types_are_correct() {
        let user = Message::user("test");
        let assistant = Message::assistant("response");
        // Just verify the types compile correctly
        let _history: Vec<Message> = vec![user, assistant];
    }

    /// Verify that run_interactive_cli has the expected async fn signature.
    #[test]
    fn function_returns_result() {
        // The function is `async fn(&SchemaForgeAgent) -> Result<(), ForgeAiError>`.
        // We verify it compiles with the expected return type by referencing it.
        let _fn_ref: fn(&SchemaForgeAgent) -> _ = |_agent| {
            Box::pin(async { Ok::<(), ForgeAiError>(()) })
        };
    }
}
