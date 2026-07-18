#!/usr/bin/env bash
# Regenerate crates/atilla-tui/src/eaw_table.rs from the vendored
# get-east-asian-width@1.6.0 lookup tables. Run from anywhere; paths are
# resolved relative to this script.
#
# Requires the vendored pi submodule with node_modules installed:
#   cd vendor/pi/packages/tui && npm install get-east-asian-width@1.6.0 --no-save --no-package-lock
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
geaw="$here/../../../../vendor/pi/node_modules/get-east-asian-width"
out="$here/../../src/eaw_table.rs"

cd "$geaw"
node --input-type=module -e '
import { fullwidthRanges, wideRanges } from "./lookup-data.js";
function toPairs(a){const p=[];for(let i=0;i<a.length;i+=2)p.push([a[i],a[i+1]]);return p;}
const all=[...toPairs(fullwidthRanges),...toPairs(wideRanges)].sort((x,y)=>x[0]-y[0]);
const merged=[];
for(const [s,e] of all){
  if(merged.length && s<=merged[merged.length-1][1]+1){merged[merged.length-1][1]=Math.max(merged[merged.length-1][1],e);}
  else merged.push([s,e]);
}
let out="// Generated from get-east-asian-width@1.6.0 (vendored at vendor/pi/node_modules).\n";
out+="// Regenerate via crates/atilla-tui/vectors/gen/gen_eaw_table.sh.\n";
out+="// Merged FullWidth + Wide codepoint ranges. eastAsianWidth(cp) returns 2 iff\n";
out+="// cp falls in one of these ranges (isFullWidth || isWide), else 1. Ambiguous,\n";
out+="// Halfwidth, Narrow and Neutral are all width 1 because pi calls the library\n";
out+="// with the default ambiguousAsWide = false.\n\n";
out+="pub(crate) const WIDE_OR_FULLWIDTH_RANGES: &[(u32, u32)] = &[\n";
for(const [s,e] of merged) out+=`    (0x${s.toString(16).toUpperCase()}, 0x${e.toString(16).toUpperCase()}),\n`;
out+="];\n";
process.stdout.write(out);
' > "$out"
cargo fmt --all --manifest-path "$here/../../../../Cargo.toml" >/dev/null 2>&1 || true
echo "wrote $out (formatted with cargo fmt)"
