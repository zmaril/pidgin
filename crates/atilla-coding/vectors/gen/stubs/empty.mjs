// straitjacket-allow-file:duplication — inert test-harness stub.
// Inert stand-in for heavy/dynamic-only deps (photon-node, jiti, hosted-git-info)
// that pi's tool/extension stack imports but the message-render vector paths
// never execute.
const noop = () => undefined;
export default new Proxy({}, { get: () => noop });
