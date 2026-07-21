// bridge_ok_gen.qnt — the deterministic driver that regenerates the committed
// happy-path trace specs/traces/bridge_ok.itf.json.
//
// It imports the real bridge_async model and pins `step` to the canonical
// sequence call(0) -> resolve(0) -> call(1) -> abort, so the exported trace is
// reproducible (no random seed) and every transition is a REAL bridge_async
// action — the ITF states are genuine states of the real model. That trace is
// replayed against the real Rust registry by the #[cfg(test)] module in
// crates/pidgin-napi/src/bridge_async.rs.
//
// Regenerate:
//   quint run --backend typescript --init initDet --step stepDet \
//     --max-steps 4 --max-samples 1 \
//     --out-itf specs/traces/bridge_ok.itf.json specs/traces/bridge_ok_gen.qnt
module bridge_ok_gen {
  import bridge_async.* from "../bridge_async"

  action initDet = init

  action stepDet = any {
    all { lastEvent == EvInit, callAsync },
    all { lastEvent == EvCall, lastId == 0, not(aborted), pending.contains(0), jsResolveValue(0) },
    all { lastEvent == EvResolveValue, lastId == 0, callAsync },
    all { lastEvent == EvCall, lastId == 1, abort },
  }
}
