// straitjacket-allow-file:duplication — inert test-harness stub.
// Inert stand-in for `highlight.js`. pi's syntax-highlight wrapper imports this
// but the message-component render paths these vectors exercise never call it
// (no valid-language code fences), so every method is a harmless no-op.
const noop = () => ({ value: "", language: undefined });
const hljs = new Proxy(
    {},
    {
        get() {
            return noop;
        },
    },
);
export default hljs;
