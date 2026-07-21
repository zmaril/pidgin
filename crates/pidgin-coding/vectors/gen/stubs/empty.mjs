const noop = () => undefined;
export default new Proxy({}, { get: () => noop });
