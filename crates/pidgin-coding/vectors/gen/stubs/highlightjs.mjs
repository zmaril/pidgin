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
