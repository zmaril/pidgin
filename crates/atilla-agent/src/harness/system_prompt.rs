//! System-prompt formatting for skills, mirroring
//! `packages/agent/src/harness/system-prompt.ts`.

use crate::harness::skills::Skill;

/// Render the model-visible `<available_skills>` block. Mirrors pi's
/// `formatSkillsForSystemPrompt`.
///
/// Skills with [`Skill::disable_model_invocation`] set are omitted; when no
/// skill is model-visible the result is an empty string.
pub fn format_skills_for_system_prompt(skills: &[Skill]) -> String {
    let visible_skills: Vec<&Skill> = skills
        .iter()
        .filter(|skill| !skill.disable_model_invocation)
        .collect();
    if visible_skills.is_empty() {
        return String::new();
    }

    let mut lines = vec![
        "The following skills provide specialized instructions for specific tasks.".to_string(),
        "Read the full skill file when the task matches its description.".to_string(),
        "When a skill file references a relative path, resolve it against the skill directory (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands.".to_string(),
        String::new(),
        "<available_skills>".to_string(),
    ];

    for skill in visible_skills {
        lines.push("  <skill>".to_string());
        lines.push(format!("    <name>{}</name>", escape_xml(&skill.name)));
        lines.push(format!(
            "    <description>{}</description>",
            escape_xml(&skill.description)
        ));
        lines.push(format!(
            "    <location>{}</location>",
            escape_xml(&skill.file_path)
        ));
        lines.push("  </skill>".to_string());
    }

    lines.push("</available_skills>".to_string());
    lines.join("\n")
}

/// Escape XML metacharacters, mirroring pi's `escapeXml` (ampersand first).
fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    // Port of `test/harness/system-prompt.test.ts`.
    use super::*;

    fn skill(
        name: &str,
        description: &str,
        content: &str,
        file_path: &str,
        disabled: bool,
    ) -> Skill {
        Skill {
            name: name.to_string(),
            description: description.to_string(),
            content: content.to_string(),
            file_path: file_path.to_string(),
            disable_model_invocation: disabled,
        }
    }

    #[test]
    fn formats_visible_skills_in_order_and_skips_model_disabled_skills() {
        let visible = skill(
            "visible",
            "Use <this> & that",
            "visible content",
            "/skills/visible/SKILL.md",
            false,
        );
        let disabled = skill(
            "hidden",
            "Hidden",
            "hidden content",
            "/skills/hidden/SKILL.md",
            true,
        );
        let second = skill(
            "second",
            "Second skill",
            "second content",
            "/skills/second/SKILL.md",
            false,
        );

        assert_eq!(
            format_skills_for_system_prompt(&[visible, disabled, second]),
            "The following skills provide specialized instructions for specific tasks.
Read the full skill file when the task matches its description.
When a skill file references a relative path, resolve it against the skill directory (parent of SKILL.md / dirname of the path) and use that absolute path in tool commands.

<available_skills>
  <skill>
    <name>visible</name>
    <description>Use &lt;this&gt; &amp; that</description>
    <location>/skills/visible/SKILL.md</location>
  </skill>
  <skill>
    <name>second</name>
    <description>Second skill</description>
    <location>/skills/second/SKILL.md</location>
  </skill>
</available_skills>"
        );
    }

    #[test]
    fn returns_an_empty_string_when_no_skills_are_model_visible() {
        let disabled = skill(
            "hidden",
            "Hidden",
            "hidden content",
            "/skills/hidden/SKILL.md",
            true,
        );
        assert_eq!(format_skills_for_system_prompt(&[disabled]), "");
    }

    #[test]
    fn escapes_xml_in_all_model_visible_skill_fields() {
        let output = format_skills_for_system_prompt(&[skill(
            "a&b",
            "Quote \"double\" and 'single'",
            "content",
            "/skills/<bad>&\"quote\"/SKILL.md",
            false,
        )]);
        assert!(output.contains(
            "<name>a&amp;b</name>\n    <description>Quote &quot;double&quot; and &apos;single&apos;</description>\n    <location>/skills/&lt;bad&gt;&amp;&quot;quote&quot;/SKILL.md</location>"
        ));
    }
}
