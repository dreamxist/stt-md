use chrono::Local;

use crate::vault::scanner::VaultVocabulary;

const MAX_FREE_TAGS: usize = 4;

const MAX_TAGS_IN_PROMPT: usize = 150;
const MAX_WIKILINKS_IN_PROMPT: usize = 100;

/// Budget for the transcript alone, leaving room in the 32k context for the
/// rules, the vault vocabulary and the answer.
const MAX_TRANSCRIPT_CHARS: usize = 80_000;

/// Whisper output grows with how much people talk, not with wall time: a dense
/// 30-min meeting runs ~40 KB. Past the context window Ollama truncates the
/// prompt from the front, which silently eats the rules and the schema above
/// the transcript and leaves the model inventing its own response shape. Drop
/// the middle instead so the instructions always survive.
fn fit_transcript(transcript: &str) -> String {
    if transcript.len() <= MAX_TRANSCRIPT_CHARS {
        return transcript.to_string();
    }

    let head_budget = MAX_TRANSCRIPT_CHARS * 3 / 5;
    let tail_budget = MAX_TRANSCRIPT_CHARS - head_budget;

    // Split on line boundaries: slicing raw bytes would panic mid-UTF-8.
    let mut head = String::with_capacity(head_budget);
    for line in transcript.lines() {
        if head.len() + line.len() + 1 > head_budget {
            break;
        }
        head.push_str(line);
        head.push('\n');
    }

    let mut tail: Vec<&str> = Vec::new();
    let mut tail_len = 0;
    for line in transcript.lines().rev() {
        if tail_len + line.len() + 1 > tail_budget {
            break;
        }
        tail_len += line.len() + 1;
        tail.push(line);
    }
    tail.reverse();

    format!(
        "{head}\n[... tramo intermedio omitido: la reunión excede el contexto del modelo ...]\n\n{}\n",
        tail.join("\n")
    )
}

pub fn build_summary_prompt(transcript: &str, vocab: &VaultVocabulary, areas: &[String]) -> String {
    let mut all_tags: Vec<String> = vocab.all_tags();
    all_tags.sort();
    all_tags.dedup();
    all_tags.truncate(MAX_TAGS_IN_PROMPT);
    let tags_str = all_tags.join(", ");

    let wikilinks: Vec<&str> = vocab
        .wikilink_targets
        .iter()
        .filter(|w| {
            w.len() < 40
                && w.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        })
        .map(|s| s.as_str())
        .take(MAX_WIKILINKS_IN_PROMPT)
        .collect();
    let wikilinks_str = wikilinks.join(", ");

    let today = Local::now();
    let today_str = today.format("%Y-%m-%d (%A)").to_string();

    let (areas_block, area_schema, area_rule, area_example) = if areas.is_empty() {
        (
            String::new(),
            "null".to_string(),
            "8. \"area\": siempre null.".to_string(),
            "null".to_string(),
        )
    } else {
        (
            format!(
                "\nÁREAS DEL VAULT (lista cerrada — elige la que mejor describe la reunión):\n{}\n",
                areas.join(", ")
            ),
            "\"una de la lista de áreas o null\"".to_string(),
            "8. \"area\": elige EXACTAMENTE un valor de la lista de áreas (el más específico que aplique). Si ninguna calza claramente, usa null.".to_string(),
            format!("\"{}\"", areas[0]),
        )
    };

    format!(
        r#"Eres un asistente que resume reuniones para un vault de Obsidian en español chileno.
Hoy es {today_str}.

VOCABULARIO DE TAGS PERMITIDOS (lista cerrada — NO existe ningún tag fuera de esta lista):
{tags_str}

WIKILINKS POSIBLES (nombres de archivos del vault — proyectos, personas, conceptos):
{wikilinks_str}
{areas_block}

REGLAS DURAS:
1. Responde EXCLUSIVAMENTE con JSON válido. Sin explicaciones, sin markdown fences.
2. Para "tags": SOLO valores de la lista de vocabulario. Si una palabra no aparece textualmente en el vocabulario, NO la pongas. Mejor lista vacía que tags inventados.
3. Para "deadline": calcula fechas relativas usando hoy = {today_short}. "Jueves próximo" = primer jueves estrictamente después de hoy. Si no hay deadline explícito, usa null.
4. "decisions" = acuerdos tomados sobre qué se va a hacer (sin quién). "action_items" = quién hace qué con deadline opcional. NO dupliques entre decisions y action_items.
5. Nombres en kebab-case lowercase sin apellido: "Juan Pérez" → "juan", "María González" → "maria".
6. NUNCA inventes personas. Si la transcripción NO menciona nombres propios explícitamente, devuelve people = []. Mejor lista vacía que nombres alucinados.
7. Si la transcripción es solo un monólogo de prueba o saludo (sin reunión real), title puede ser "Nota rápida", summary breve, decisions/action_items/people vacíos.
{area_rule}
9. Si las líneas traen prefijo de hablante: "yo:" es el usuario local (dueño de esta nota) y "ellos:" son los demás participantes. Úsalo para atribuir decisiones y "who" en action_items: lo que dice "yo" comprometerse a hacer es del usuario local; lo que "ellos" se comprometen es de la persona correspondiente.

SCHEMA EXACTO:
{{
  "title": "string corto sin fecha (ej: 'Acme standup')",
  "summary_md": "markdown con 4-7 bullets sobre lo principal",
  "decisions": ["frases cortas, una por decisión"],
  "action_items": [
    {{ "who": "kebab-case o null", "task": "string", "deadline": "YYYY-MM-DD o null" }}
  ],
  "people": ["kebab-case lowercase"],
  "tags": ["solo del vocabulario"],
  "project_wikilink": "[[nombre]] o null",
  "area": {area_schema}
}}

EJEMPLO de buena respuesta:
{{
  "title": "Sync proyecto-x sobre roadmap",
  "summary_md": "- Avanzamos en la fase de discovery\n- Faltan validar requisitos con stakeholders",
  "decisions": ["Se acordó priorizar el flow A sobre el B"],
  "action_items": [
    {{ "who": "ana", "task": "Documentar los requisitos funcionales", "deadline": "2026-04-30" }}
  ],
  "people": ["ana", "luis"],
  "tags": ["proyecto-x", "roadmap"],
  "project_wikilink": "[[proyecto-x]]",
  "area": {area_example}
}}

TRANSCRIPCIÓN A RESUMIR:
{transcript}

JSON:"#,
        today_str = today_str,
        today_short = today.format("%Y-%m-%d"),
        tags_str = tags_str,
        wikilinks_str = wikilinks_str,
        transcript = fit_transcript(transcript),
    )
}

/// Prompt for `output_mode = "simple"` — no vault, no closed vocabulary.
/// LLM picks up to MAX_FREE_TAGS tags freely (lowercase kebab-case).
pub fn build_simple_summary_prompt(transcript: &str) -> String {
    let today = Local::now();
    let today_str = today.format("%Y-%m-%d (%A)").to_string();
    let today_short = today.format("%Y-%m-%d").to_string();

    format!(
        r#"Eres un asistente que resume reuniones en español chileno neutro.
Hoy es {today_str}.

REGLAS DURAS:
1. Responde EXCLUSIVAMENTE con JSON válido. Sin explicaciones, sin markdown fences.
2. "deadline": fechas relativas usando hoy = {today_short}. "Jueves próximo" = primer jueves estrictamente después de hoy. Si no hay deadline explícito, usa null. Solo formato YYYY-MM-DD.
3. "decisions" = acuerdos tomados sobre qué se va a hacer (sin quién). "action_items" = quién hace qué con deadline opcional. NO dupliques entre decisions y action_items.
4. Nombres en kebab-case lowercase sin apellido: "Juan Pérez" → "juan", "María González" → "maria".
5. NUNCA inventes personas. Si no se mencionan nombres propios explícitamente, devuelve people = [].
6. "tags": máximo {max_tags} tags en kebab-case lowercase, descriptivos del contenido (ej: "standup", "planning", "retro", "1on1"). Sin tildes ni símbolos.
7. project_wikilink: siempre null en este modo.
8. Si la transcripción es solo una nota rápida o saludo (sin reunión real), title puede ser "Nota rápida", listas vacías.
9. Si las líneas traen prefijo de hablante: "yo:" es el usuario local (dueño de esta nota) y "ellos:" son los demás participantes. Úsalo para atribuir decisiones y "who" en action_items.

SCHEMA EXACTO:
{{
  "title": "string corto sin fecha",
  "summary_md": "markdown con 4-7 bullets sobre lo principal",
  "decisions": ["frases cortas"],
  "action_items": [
    {{ "who": "kebab-case o null", "task": "string", "deadline": "YYYY-MM-DD o null" }}
  ],
  "people": ["kebab-case lowercase"],
  "tags": ["máximo {max_tags} tags kebab-case"],
  "project_wikilink": null
}}

TRANSCRIPCIÓN A RESUMIR:
{transcript}

JSON:"#,
        today_str = today_str,
        today_short = today_short,
        max_tags = MAX_FREE_TAGS,
        transcript = fit_transcript(transcript),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transcript_of(lines: usize) -> String {
        (0..lines)
            .map(|i| format!("[{:02}:{:02}] línea número {i} de la reunión\n", i / 60, i % 60))
            .collect()
    }

    #[test]
    fn short_transcript_passes_through_untouched() {
        let t = transcript_of(100);
        assert!(t.len() < MAX_TRANSCRIPT_CHARS);
        assert_eq!(fit_transcript(&t), t);
    }

    #[test]
    fn long_transcript_keeps_both_ends_within_budget() {
        let t = transcript_of(4_000);
        assert!(t.len() > MAX_TRANSCRIPT_CHARS);

        let fitted = fit_transcript(&t);
        assert!(fitted.len() <= MAX_TRANSCRIPT_CHARS + 200);
        assert!(fitted.contains("línea número 0 "));
        assert!(fitted.contains("línea número 3999 "));
        assert!(fitted.contains("tramo intermedio omitido"));
    }

    #[test]
    fn rules_survive_for_a_transcript_that_used_to_blow_the_context() {
        let prompt = build_summary_prompt(&transcript_of(4_000), &VaultVocabulary::default(), &[]);
        assert!(prompt.starts_with("Eres un asistente"));
        assert!(prompt.contains("REGLAS DURAS"));
        assert!(prompt.contains("SCHEMA EXACTO"));
    }
}
