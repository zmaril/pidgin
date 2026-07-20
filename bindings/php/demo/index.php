<?php
// index.php — a plain-PHP chat webpage that runs pi natively through the
// `Pidgin\Session` extension. No framework.
//
// This file assumes the pidgin-php extension is already loaded into the PHP
// process — it does NOT hardcode a .so path. The extension is loaded by the
// launcher (`serve.sh`) via `php -d extension=<abs-path>/libpidgin_php.so`, or
// permanently via php.ini. See demo/README.md.
//
// Mode selection: LIVE when ANTHROPIC_API_KEY is present in the environment,
// FAUX (offline, deterministic echo) when it is absent.
//
// NOTE: this web demo creates one `Pidgin\Session` per request, which is the
// simplest thing given PHP's request-isolated built-in server. Per-Session
// multi-turn context (a single Session carried across several send() calls) is
// demonstrated in test.php, not here — each browser message here is its own
// fresh Session.

declare(strict_types=1);

$hasKey = getenv('ANTHROPIC_API_KEY') !== false && getenv('ANTHROPIC_API_KEY') !== '';
$mode = $hasKey ? 'LIVE' : 'FAUX';

// ---------------------------------------------------------------------------
// POST: run one agent turn through the extension and return JSON.
// ---------------------------------------------------------------------------
if ($_SERVER['REQUEST_METHOD'] === 'POST') {
    header('Content-Type: application/json');

    // Guard: the extension must be loaded for the Session class to exist.
    if (!class_exists('Pidgin\\Session')) {
        http_response_code(500);
        echo json_encode([
            'error' => 'The pidgin-php extension is not loaded: class '
                . 'Pidgin\\Session is missing. Start the server with '
                . 'serve.sh (it passes -d extension=<abs>/libpidgin_php.so), '
                . 'or add the extension to your php.ini.',
        ]);
        exit;
    }

    // Accept either a JSON body ({"message": "..."}) or a form-encoded field.
    $raw = file_get_contents('php://input');
    $message = null;
    if ($raw !== false && $raw !== '') {
        $decoded = json_decode($raw, true);
        if (is_array($decoded) && isset($decoded['message'])) {
            $message = (string) $decoded['message'];
        }
    }
    if ($message === null && isset($_POST['message'])) {
        $message = (string) $_POST['message'];
    }

    if ($message === null || $message === '') {
        http_response_code(400);
        echo json_encode(['error' => 'Empty message.']);
        exit;
    }

    try {
        // faux (4th arg) is true when there is no API key -> offline echo path.
        // Args are positional: ext-php-rs 0.13.1 does not support named-arg
        // skipping of the Option params.
        $faux = !$hasKey;
        $session = new Pidgin\Session(null, null, null, $faux);
        $reply = $session->send($message);
        echo json_encode(['reply' => $reply]);
    } catch (\Throwable $e) {
        http_response_code(500);
        echo json_encode(['error' => $e->getMessage()]);
    }
    exit;
}

// ---------------------------------------------------------------------------
// GET: serve the chat page.
// ---------------------------------------------------------------------------
$extLoaded = class_exists('Pidgin\\Session');
?>
<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>pidgin - PHP running pi natively</title>
<style>
  :root {
    --bg: #f6f7f9;
    --panel: #ffffff;
    --ink: #1b1f23;
    --muted: #6b7280;
    --line: #e2e5e9;
    --user: #2563eb;
    --assistant: #10202e;
    --accent: #0f766e;
  }
  * { box-sizing: border-box; }
  body {
    margin: 0;
    background: var(--bg);
    color: var(--ink);
    font: 15px/1.5 ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, monospace;
    display: flex;
    justify-content: center;
  }
  main {
    width: 100%;
    max-width: 720px;
    padding: 24px 16px 32px;
    display: flex;
    flex-direction: column;
    min-height: 100vh;
  }
  header { margin-bottom: 16px; }
  h1 { font-size: 18px; margin: 0 0 6px; }
  .sub { color: var(--muted); font-size: 13px; }
  .badge {
    display: inline-block;
    padding: 2px 8px;
    border-radius: 999px;
    font-size: 12px;
    font-weight: 700;
    letter-spacing: 0.04em;
    vertical-align: middle;
    margin-left: 6px;
  }
  .badge.faux { background: #fff4d6; color: #8a5a00; border: 1px solid #f0d488; }
  .badge.live { background: #dcfce7; color: #14532d; border: 1px solid #86efac; }
  .warn {
    background: #fee2e2; border: 1px solid #fca5a5; color: #7f1d1d;
    padding: 8px 12px; border-radius: 8px; font-size: 13px; margin-bottom: 12px;
  }
  #transcript {
    flex: 1;
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 10px;
    padding: 14px;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 10px;
    min-height: 240px;
  }
  .bubble {
    max-width: 85%;
    padding: 8px 12px;
    border-radius: 12px;
    white-space: pre-wrap;
    word-wrap: break-word;
  }
  .bubble.user {
    align-self: flex-end;
    background: var(--user);
    color: #fff;
    border-bottom-right-radius: 3px;
  }
  .bubble.assistant {
    align-self: flex-start;
    background: #eef1f4;
    color: var(--assistant);
    border-bottom-left-radius: 3px;
  }
  .bubble.error {
    align-self: flex-start;
    background: #fee2e2;
    color: #7f1d1d;
    border: 1px solid #fca5a5;
  }
  .who { display: block; font-size: 11px; color: var(--muted); margin-bottom: 2px; }
  form { display: flex; gap: 8px; margin-top: 12px; }
  #msg {
    flex: 1;
    padding: 10px 12px;
    border: 1px solid var(--line);
    border-radius: 8px;
    font: inherit;
    background: var(--panel);
    color: var(--ink);
  }
  button {
    padding: 10px 18px;
    border: none;
    border-radius: 8px;
    background: var(--accent);
    color: #fff;
    font: inherit;
    font-weight: 700;
    cursor: pointer;
  }
  button:disabled { opacity: 0.55; cursor: default; }
  .empty { color: var(--muted); font-size: 13px; }
</style>
</head>
<body>
<main>
  <header>
    <h1>pidgin <span class="sub">- PHP running pi natively</span>
      <span class="badge <?= $mode === 'LIVE' ? 'live' : 'faux' ?>"><?= $mode ?></span>
    </h1>
    <div class="sub">
      <?php if ($mode === 'LIVE'): ?>
        Live mode: messages hit the real Anthropic API through the native transport.
      <?php else: ?>
        Faux mode (offline): no API key set, replies are deterministic echoes.
      <?php endif; ?>
    </div>
  </header>

  <?php if (!$extLoaded): ?>
    <div class="warn">
      The pidgin-php extension is not loaded (class Pidgin\Session is missing).
      Start this page via demo/serve.sh so the .so is passed with -d extension=...
    </div>
  <?php endif; ?>

  <div id="transcript">
    <div class="empty">Send a message to run a turn through the extension.</div>
  </div>

  <form id="chat" autocomplete="off">
    <input id="msg" name="message" placeholder="Type a message and press Send" required>
    <button id="send" type="submit">Send</button>
  </form>
</main>

<script>
  const transcript = document.getElementById('transcript');
  const form = document.getElementById('chat');
  const input = document.getElementById('msg');
  const button = document.getElementById('send');

  function addBubble(kind, who, text) {
    const empty = transcript.querySelector('.empty');
    if (empty) empty.remove();
    const b = document.createElement('div');
    b.className = 'bubble ' + kind;
    const w = document.createElement('span');
    w.className = 'who';
    w.textContent = who;
    b.appendChild(w);
    b.appendChild(document.createTextNode(text));
    transcript.appendChild(b);
    transcript.scrollTop = transcript.scrollHeight;
    return b;
  }

  form.addEventListener('submit', async (e) => {
    e.preventDefault();
    const message = input.value.trim();
    if (!message) return;

    addBubble('user', 'you', message);
    input.value = '';
    input.disabled = true;
    button.disabled = true;

    const pending = addBubble('assistant', 'pidgin', '...');

    try {
      const res = await fetch(window.location.pathname, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ message }),
      });
      const data = await res.json().catch(() => ({ error: 'Invalid JSON from server.' }));
      if (!res.ok || data.error) {
        pending.remove();
        addBubble('error', 'error', data.error || ('HTTP ' + res.status));
      } else {
        pending.textContent = '';
        const w = document.createElement('span');
        w.className = 'who';
        w.textContent = 'pidgin';
        pending.appendChild(w);
        pending.appendChild(document.createTextNode(data.reply));
      }
    } catch (err) {
      pending.remove();
      addBubble('error', 'error', String(err));
    } finally {
      input.disabled = false;
      button.disabled = false;
      input.focus();
    }
  });
</script>
</body>
</html>
