// straitjacket-allow-file:duplication — one-line loader registration shim.
// Registers ./loader.mjs as an ESM resolve hook (Node >=20.6 module.register).
import { register } from "node:module";
register("./loader.mjs", import.meta.url);
