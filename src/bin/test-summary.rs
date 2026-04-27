use anyhow::Result;
use std::path::PathBuf;
use std::time::Instant;

use stt_md::{llm, vault};

fn main() -> Result<()> {
    let vault_path = std::env::var("STT_MD_VAULT")
        .map(PathBuf::from)
        .map_err(|_| anyhow::anyhow!("set STT_MD_VAULT to point at your Obsidian vault root"))?;
    println!("scanning vault at {}", vault_path.display());
    let t0 = Instant::now();
    let vocab = vault::scanner::scan_vault(&vault_path)?;
    println!(
        "  scanned in {}ms — {} frontmatter tags, {} inline tags, {} wikilink targets",
        t0.elapsed().as_millis(),
        vocab.frontmatter_tags.len(),
        vocab.inline_tags.len(),
        vocab.wikilink_targets.len()
    );
    println!("\nfrontmatter tags: {:?}", vocab.frontmatter_tags);
    println!("\ninline tags: {:?}", vocab.inline_tags);

    let fake_transcript = r#"
[00:00] Pancho: Ok, vamos al standup de HeyMark. Juan, ¿cómo viene el onboarding?
[00:05] Juan: Bien, terminé el flow base, falta validar con María el copy.
[00:12] María: Hoy le mando los textos finales a Juan en la tarde.
[00:18] Pancho: Bacán. Para el sprint que viene priorizamos integración con Linear para tracking de bugs.
[00:25] Juan: Sí, eso queda para mí, deadline jueves próximo.
[00:32] Pancho: Tema importante, hay que migrar el endpoint de auth antes del 5 de mayo, riesgo de que rompa producción si no.
[00:42] María: Yo me encargo de avisar al cliente HeyMark sobre el cambio.
[00:50] Pancho: Listo. Última cosa, María, ¿cómo va el rediseño del feed?
[00:58] María: Tengo el primer draft, te lo paso mañana para review.
[01:05] Pancho: Perfecto. Cerramos.
"#;

    let prompt = llm::prompts::build_summary_prompt(fake_transcript, &vocab);
    println!("\n--- PROMPT ({} chars) ---", prompt.len());

    println!("\ncalling ollama (qwen2.5:7b)...");
    let t0 = Instant::now();
    let json = llm::ollama::generate_json(
        &prompt,
        llm::ollama::DEFAULT_MODEL,
        llm::ollama::DEFAULT_URL,
    )?;
    println!("got response in {}ms", t0.elapsed().as_millis());
    println!("\n--- RAW RESPONSE ---\n{json}\n");

    match serde_json::from_str::<llm::MeetingSummary>(&json) {
        Ok(mut parsed) => {
            println!("--- PARSED (raw) ---\n{parsed:#?}");
            parsed.enforce_vocab(&vocab);
            println!("\n--- AFTER VOCAB ENFORCEMENT ---\ntags: {:?}\nproject: {:?}", parsed.tags, parsed.project_wikilink);
        }
        Err(e) => {
            eprintln!("FAILED to parse JSON: {e}");
            std::process::exit(2);
        }
    }

    Ok(())
}
