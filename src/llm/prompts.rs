use chrono::Local;

use crate::vault::scanner::VaultVocabulary;

const MAX_TAGS_IN_PROMPT: usize = 150;
const MAX_WIKILINKS_IN_PROMPT: usize = 100;

pub fn build_summary_prompt(transcript: &str, vocab: &VaultVocabulary) -> String {
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

    format!(
        r#"Eres un asistente que resume reuniones para un vault de Obsidian en español chileno.
Hoy es {today_str}.

VOCABULARIO DE TAGS PERMITIDOS (lista cerrada — NO existe ningún tag fuera de esta lista):
{tags_str}

WIKILINKS POSIBLES (nombres de archivos del vault — proyectos, personas, conceptos):
{wikilinks_str}

REGLAS DURAS:
1. Responde EXCLUSIVAMENTE con JSON válido. Sin explicaciones, sin markdown fences.
2. Para "tags": SOLO valores de la lista de vocabulario. Si una palabra no aparece textualmente en el vocabulario, NO la pongas. Mejor lista vacía que tags inventados.
3. Para "deadline": calcula fechas relativas usando hoy = {today_short}. "Jueves próximo" = primer jueves estrictamente después de hoy. Si no hay deadline explícito, usa null.
4. "decisions" = acuerdos tomados sobre qué se va a hacer (sin quién). "action_items" = quién hace qué con deadline opcional. NO dupliques entre decisions y action_items.
5. Nombres en kebab-case lowercase sin apellido: "Juan Pérez" → "juan", "María González" → "maria".
6. NUNCA inventes personas. Si la transcripción NO menciona nombres propios explícitamente, devuelve people = []. Mejor lista vacía que nombres alucinados.
7. Si la transcripción es solo un monólogo de prueba o saludo (sin reunión real), title puede ser "Nota rápida", summary breve, decisions/action_items/people vacíos.

SCHEMA EXACTO:
{{
  "title": "string corto sin fecha (ej: 'HeyMark standup')",
  "summary_md": "markdown con 4-7 bullets sobre lo principal",
  "decisions": ["frases cortas, una por decisión"],
  "action_items": [
    {{ "who": "kebab-case o null", "task": "string", "deadline": "YYYY-MM-DD o null" }}
  ],
  "people": ["kebab-case lowercase"],
  "tags": ["solo del vocabulario"],
  "project_wikilink": "[[nombre]] o null"
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
  "project_wikilink": "[[proyecto-x]]"
}}

TRANSCRIPCIÓN A RESUMIR:
{transcript}

JSON:"#,
        today_str = today_str,
        today_short = today.format("%Y-%m-%d"),
        tags_str = tags_str,
        wikilinks_str = wikilinks_str,
        transcript = transcript,
    )
}
