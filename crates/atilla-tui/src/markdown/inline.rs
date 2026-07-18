// straitjacket-allow-file:duplication — the per-tokenizer dispatch arms in
// `inline_tokens` are a faithful 1:1 mirror of marked's `Lexer.inlineTokens`
// try-each-rule loop; their shared shape is inherent to the parallel port.
//! Inline tokenizer half of the marked lexer port (see `lexer.rs`). Split out
//! of `lexer.rs` to keep each file within the file-size budget. The inline
//! grammar (escape / html tag / link / emphasis / codespan / br / strict
//! strikethrough / autolink / bare-url / text) plus the masking pass are
//! ported from marked's gfm inline rule set.

use super::lexer::{is_match, rtrim, rx, Kind, Lexer, Token};

impl Lexer {
    pub fn inline_tokens(&mut self, src: &str) -> Vec<Token> {
        let masked = self.build_masked(src);
        let mut tokens: Vec<Token> = Vec::new();
        let mut rest = src.to_string();
        let mut prev_char = String::new();
        let mut keep_prev = false;
        let mut last_len = usize::MAX;
        while !rest.is_empty() {
            if rest.len() < last_len {
                last_len = rest.len();
            } else {
                break;
            }
            if !keep_prev {
                prev_char = String::new();
            }
            keep_prev = false;

            if let Some(tok) = self.i_escape(&rest) {
                rest = rest[tok.raw.len()..].to_string();
                tokens.push(tok);
                continue;
            }
            if let Some(tok) = self.i_tag(&rest) {
                rest = rest[tok.raw.len()..].to_string();
                tokens.push(tok);
                continue;
            }
            if let Some(tok) = self.i_link(&rest) {
                rest = rest[tok.raw.len()..].to_string();
                tokens.push(tok);
                continue;
            }
            if let Some(tok) = self.i_em_strong(&rest, &masked, &prev_char) {
                rest = rest[tok.raw.len()..].to_string();
                tokens.push(tok);
                continue;
            }
            if let Some(tok) = self.i_codespan(&rest) {
                rest = rest[tok.raw.len()..].to_string();
                tokens.push(tok);
                continue;
            }
            if let Some(tok) = self.i_br(&rest) {
                rest = rest[tok.raw.len()..].to_string();
                tokens.push(tok);
                continue;
            }
            if let Some(tok) = self.i_del(&rest) {
                rest = rest[tok.raw.len()..].to_string();
                tokens.push(tok);
                continue;
            }
            if let Some(tok) = self.i_autolink(&rest) {
                rest = rest[tok.raw.len()..].to_string();
                tokens.push(tok);
                continue;
            }
            if !self.state_in_link {
                if let Some(tok) = self.i_url(&rest) {
                    rest = rest[tok.raw.len()..].to_string();
                    tokens.push(tok);
                    continue;
                }
            }
            if let Some(tok) = self.i_inline_text(&rest) {
                let raw_len = tok.raw.len();
                let last_char = tok.raw.chars().last().unwrap_or(' ');
                if last_char != '_' {
                    prev_char = last_char.to_string();
                }
                keep_prev = true;
                rest = rest[raw_len..].to_string();
                if let Some(last) = tokens.last_mut() {
                    if last.kind == Kind::Text {
                        last.raw.push_str(&tok.raw);
                        last.text.push_str(&tok.text);
                        continue;
                    }
                }
                tokens.push(tok);
                continue;
            }
            break;
        }
        tokens
    }

    /// marked's masking pass: escape-punct -> `++`, then block-skip link/code/
    /// html spans -> `[aaa]`, all length-preserving so R-delim indices align.
    fn build_masked(&self, src: &str) -> String {
        // anyPunctuation (global): `\X` -> `++` (length-preserving).
        let mut n = String::new();
        {
            let mut pos = 0usize;
            let mut base = 0usize;
            while let Ok(Some(cap)) = self.rules.i_any_punctuation.captures_from_pos(src, base) {
                let m = cap.get(0).unwrap();
                n.push_str(&src[pos..m.start()]);
                n.push_str("++");
                pos = m.end();
                base = m.end();
            }
            n.push_str(&src[pos..]);
        }
        // blockSkip (global): replace link/codespan/html span with `[aaa]`.
        let mut out = String::new();
        {
            let mut pos = 0usize;
            let mut base = 0usize;
            while let Ok(Some(cap)) = self.rules.i_block_skip.captures_from_pos(&n, base) {
                let m = cap.get(0).unwrap();
                let r = cap.get(2).map(|g| g.as_str().len()).unwrap_or(0);
                out.push_str(&n[pos..m.start() + r]);
                let inner = m.as_str().len().saturating_sub(r).saturating_sub(2);
                out.push('[');
                out.push_str(&"a".repeat(inner));
                out.push(']');
                pos = m.end();
                base = m.end();
                if base > n.len() {
                    break;
                }
            }
            out.push_str(&n[pos..]);
        }
        out
    }

    fn i_escape(&self, src: &str) -> Option<Token> {
        let caps = self.rules.i_escape.captures(src).ok().flatten()?;
        let mut tok = Token::new(Kind::Escape);
        tok.raw = caps.get(0)?.as_str().to_string();
        tok.text = caps.get(1)?.as_str().to_string();
        Some(tok)
    }

    fn i_tag(&mut self, src: &str) -> Option<Token> {
        let caps = self.rules.i_tag.captures(src).ok().flatten()?;
        let raw = caps.get(0)?.as_str().to_string();
        if !self.state_in_link && is_match(&self.rules.o_start_a_tag, &raw) {
            self.state_in_link = true;
        } else if self.state_in_link && is_match(&self.rules.o_end_a_tag, &raw) {
            self.state_in_link = false;
        }
        if !self.state_in_raw_block && is_match(&self.rules.o_start_pre_script, &raw) {
            self.state_in_raw_block = true;
        } else if self.state_in_raw_block && is_match(&self.rules.o_end_pre_script, &raw) {
            self.state_in_raw_block = false;
        }
        let mut tok = Token::new(Kind::Html);
        tok.raw = raw.clone();
        tok.text = raw;
        Some(tok)
    }

    fn i_link(&mut self, src: &str) -> Option<Token> {
        let caps = self.rules.i_link.captures(src).ok().flatten()?;
        let cap0 = caps.get(0)?.as_str().to_string();
        let cap1 = caps.get(1).map(|m| m.as_str()).unwrap_or("").to_string();
        let mut href_field = caps.get(2).map(|m| m.as_str()).unwrap_or("").to_string();

        let trimmed = href_field.trim().to_string();
        if is_match(&self.rules.o_start_angle, &trimmed) {
            if !trimmed.ends_with('>') {
                return None;
            }
            let inner = &trimmed[..trimmed.len() - 1];
            let bs = rtrim(inner, '\\', true);
            if (trimmed.chars().count() - bs.chars().count()).is_multiple_of(2) {
                return None;
            }
        }
        let mut href = href_field.clone();
        href = href.trim().to_string();
        if is_match(&self.rules.o_start_angle, &href) {
            href = href[1..href.len() - 1].to_string();
        }
        href = self
            .rules
            .i_any_punctuation
            .replace_all(&href, "$1")
            .into_owned();
        let _ = &mut href_field;
        Some(output_link(self, &cap0, &cap1, href, &cap0))
    }

    fn i_em_strong(&mut self, src: &str, masked: &str, prev_char: &str) -> Option<Token> {
        let lcaps = self.rules.i_em_ldelim.captures(src).ok().flatten()?;
        let g1 = lcaps.get(1).map(|m| m.as_str());
        let g2 = lcaps.get(2).map(|m| m.as_str());
        let g3 = lcaps.get(3).map(|m| m.as_str());
        let g4 = lcaps.get(4).map(|m| m.as_str());
        if g1.is_none() && g2.is_none() && g3.is_none() && g4.is_none() {
            return None;
        }
        if g4.is_some() && prev_char_is_alnum(prev_char) {
            return None;
        }
        let l0 = lcaps.get(0)?.as_str();
        let cond = g1.is_none() && g3.is_none();
        if !(cond || prev_char.is_empty() || is_match(&self.rules.i_punctuation, prev_char)) {
            return None;
        }
        let l_length = l0.chars().count() as i64 - 1;
        let first_ch = l0.chars().next()?;
        let end_re = if first_ch == '*' {
            &self.rules.i_em_rdelim_ast
        } else {
            &self.rules.i_em_rdelim_und
        };

        // maskedSrc.slice(-src.length + lLength)
        let masked_chars: Vec<char> = masked.chars().collect();
        let src_len = src.chars().count() as i64;
        let start_idx = (masked_chars.len() as i64 - src_len + l_length).max(0) as usize;
        let scan: String = masked_chars[start_idx.min(masked_chars.len())..]
            .iter()
            .collect();

        let mut delim_total = l_length;
        let mut mid_delim_total = 0i64;
        let mut pos = 0usize;
        while let Some(mcaps) = end_re.captures_from_pos(&scan, pos).ok().flatten() {
            let m0 = mcaps.get(0).unwrap();
            let rdelim = (1..=6)
                .filter_map(|i| mcaps.get(i))
                .map(|m| m.as_str())
                .find(|s| !s.is_empty());
            let next_pos = if m0.end() > pos { m0.end() } else { pos + 1 };
            let rdelim = match rdelim {
                Some(r) => r,
                None => {
                    pos = next_pos;
                    continue;
                }
            };
            let r_length = rdelim.chars().count() as i64;
            let has_g3 = mcaps
                .get(3)
                .map(|m| !m.as_str().is_empty())
                .unwrap_or(false);
            let has_g4 = mcaps
                .get(4)
                .map(|m| !m.as_str().is_empty())
                .unwrap_or(false);
            if has_g3 || has_g4 {
                delim_total += r_length;
                pos = next_pos;
                continue;
            }
            let has_g5 = mcaps
                .get(5)
                .map(|m| !m.as_str().is_empty())
                .unwrap_or(false);
            let has_g6 = mcaps
                .get(6)
                .map(|m| !m.as_str().is_empty())
                .unwrap_or(false);
            if (has_g5 || has_g6) && l_length % 3 != 0 && (l_length + r_length) % 3 == 0 {
                mid_delim_total += r_length;
                pos = next_pos;
                continue;
            }
            delim_total -= r_length;
            if delim_total > 0 {
                pos = next_pos;
                continue;
            }
            let r_use = r_length.min(r_length + delim_total + mid_delim_total);
            let m0_first_len = i64::from(m0.as_str().chars().next().is_some());
            let m_index = scan[..m0.start()].chars().count() as i64;
            let raw_len = (l_length + m_index + m0_first_len + r_use) as usize;
            let src_chars: Vec<char> = src.chars().collect();
            let raw: String = src_chars[..raw_len.min(src_chars.len())].iter().collect();
            if l_length.min(r_use) % 2 != 0 {
                let text: String = raw.chars().skip(1).take(raw.chars().count() - 2).collect();
                let mut tok = Token::new(Kind::Em);
                tok.raw = raw;
                tok.tokens = self.inline_tokens(&text);
                tok.text = text;
                return Some(tok);
            }
            let text: String = raw.chars().skip(2).take(raw.chars().count() - 4).collect();
            let mut tok = Token::new(Kind::Strong);
            tok.raw = raw;
            tok.tokens = self.inline_tokens(&text);
            tok.text = text;
            return Some(tok);
        }
        None
    }

    fn i_codespan(&self, src: &str) -> Option<Token> {
        let caps = self.rules.i_code.captures(src).ok().flatten()?;
        let raw = caps.get(0)?.as_str().to_string();
        let mut text = caps.get(2)?.as_str().replace('\n', " ");
        let has_non_space = is_match(&self.rules.o_non_space_char, &text);
        let starts_space = text.starts_with(' ');
        let ends_space = text.ends_with(' ');
        if has_non_space && starts_space && ends_space {
            text = text[1..text.len() - 1].to_string();
        }
        let mut tok = Token::new(Kind::Codespan);
        tok.raw = raw;
        tok.text = text;
        Some(tok)
    }

    fn i_br(&self, src: &str) -> Option<Token> {
        let caps = self.rules.i_br.captures(src).ok().flatten()?;
        let mut tok = Token::new(Kind::Br);
        tok.raw = caps.get(0)?.as_str().to_string();
        Some(tok)
    }

    fn i_del(&mut self, src: &str) -> Option<Token> {
        let caps = self.rules.i_del_strict.captures(src).ok().flatten()?;
        let raw = caps.get(0)?.as_str().to_string();
        let text = caps.get(2)?.as_str().to_string();
        let mut tok = Token::new(Kind::Del);
        tok.raw = raw;
        tok.tokens = self.inline_tokens(&text);
        tok.text = text;
        Some(tok)
    }

    fn i_autolink(&mut self, src: &str) -> Option<Token> {
        let caps = self.rules.i_autolink.captures(src).ok().flatten()?;
        let raw = caps.get(0)?.as_str().to_string();
        let inner = caps.get(1)?.as_str().to_string();
        let is_email = caps.get(2).is_some();
        let (text, href) = if is_email {
            (inner.clone(), format!("mailto:{inner}"))
        } else {
            (inner.clone(), inner.clone())
        };
        Some(make_autolink(raw, text, href))
    }

    fn i_url(&mut self, src: &str) -> Option<Token> {
        let caps = self.rules.i_url.captures(src).ok().flatten()?;
        let m0 = caps.get(0)?.as_str().to_string();
        let is_email = caps.get(2).is_some();
        if is_email {
            let text = m0.clone();
            let href = format!("mailto:{m0}");
            return Some(make_autolink(m0, text, href));
        }
        // backpedal trailing punctuation for bare URLs
        let mut cur = m0.clone();
        loop {
            let prev = cur.clone();
            cur = backpedal(&prev);
            if prev == cur {
                break;
            }
        }
        let scheme_www = caps.get(1).map(|m| m.as_str()).unwrap_or("");
        let href = if scheme_www == "www." {
            format!("http://{cur}")
        } else {
            cur.clone()
        };
        Some(make_autolink(cur.clone(), cur, href))
    }

    fn i_inline_text(&self, src: &str) -> Option<Token> {
        let caps = self.rules.i_text.captures(src).ok().flatten()?;
        let raw = caps.get(0)?.as_str().to_string();
        let mut tok = Token::new(Kind::Text);
        tok.text = raw.clone();
        tok.raw = raw;
        Some(tok)
    }
}

fn output_link(lex: &mut Lexer, cap0: &str, cap1: &str, href: String, raw: &str) -> Token {
    let text = lex
        .rules
        .o_output_link_replace
        .replace_all(cap1, "$1")
        .into_owned();
    lex.state_in_link = true;
    let mut tok = Token::new(Kind::Link);
    tok.raw = raw.to_string();
    tok.href = href;
    tok.text = text.clone();
    tok.tokens = lex.inline_tokens(&text);
    let _ = cap0;
    lex.state_in_link = false;
    tok
}

fn make_autolink(raw: String, text: String, href: String) -> Token {
    let mut tok = Token::new(Kind::Link);
    tok.raw = raw;
    tok.href = href;
    let mut inner = Token::new(Kind::Text);
    inner.raw = text.clone();
    inner.text = text.clone();
    tok.tokens = vec![inner];
    tok.text = text;
    tok
}

fn backpedal(url: &str) -> String {
    let re =
        rx(r"(?:[^?!.,:;*_'\x22~()&]+|\([^)]*\)|&(?![a-zA-Z0-9]+;$)|[?!.,:;*_'\x22~)]+(?!$))+");
    match re.captures(url).ok().flatten() {
        Some(c) => c.get(0).map(|m| m.as_str().to_string()).unwrap_or_default(),
        None => String::new(),
    }
}

fn prev_char_is_alnum(s: &str) -> bool {
    let re = rx(r"[\p{L}\p{N}]");
    is_match(&re, s)
}
