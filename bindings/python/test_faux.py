#!/usr/bin/env python3
"""Offline smoke test for the pidgin Python native extension.

Exercises the whole surface against the faux provider: NO network, NO API key.
Prints PASS lines and exits non-zero on any failure.
"""

import sys

import pidgin


def check(condition, message):
    if not condition:
        print(f"FAIL: {message}", file=sys.stderr)
        sys.exit(1)
    print(f"PASS: {message}")


def main():
    # 1. version() is a non-empty str.
    ver = pidgin.version()
    check(isinstance(ver, str) and len(ver) > 0, f"pidgin.version() -> {ver!r}")

    # 2. Construct an offline faux session.
    session = pidgin.Session(faux=True)
    check(session is not None, "pidgin.Session(faux=True) constructed")

    # 3. send() returns a non-empty str that echoes the prompt.
    prompt = "hello from python"
    reply = session.send(prompt)
    check(isinstance(reply, str) and len(reply) > 0, f"send() -> {len(reply)} chars")
    check(prompt in reply, f"send() reply echoes the prompt {prompt!r}")
    check(
        "offline faux assistant" in reply,
        "send() reply contains the faux marker text",
    )

    # 4. send_stream() yields >= 1 chunk; joined chunks contain the reply text.
    stream = session.send_stream("streamed hello")
    chunks = list(stream)
    check(len(chunks) >= 1, f"send_stream() yielded {len(chunks)} chunk(s)")
    check(
        all(isinstance(c, str) for c in chunks),
        "send_stream() chunks are all str",
    )
    joined = "".join(chunks)
    check(len(joined) > 0, f"send_stream() joined text -> {len(joined)} chars")
    check(
        "streamed hello" in joined,
        "send_stream() joined text echoes the prompt",
    )
    check(
        "offline faux assistant" in joined,
        "send_stream() joined text contains the faux marker text",
    )

    print("\nALL PASS")


if __name__ == "__main__":
    main()
