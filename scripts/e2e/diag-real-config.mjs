// 只读诊断：用户真实配置里 订阅 / 线路(lanes) / 通道 的实际形态（不写任何数据）
import { DatabaseSync } from "node:sqlite";
import { existsSync } from "node:fs";
import { join } from "node:path";

const dir = join(process.env.APPDATA ?? "", "TokenAgency");
const p = join(dir, "db.sqlite");
console.log("db =", p, "exists =", existsSync(p));
if (!existsSync(p)) process.exit(0);

const db = new DatabaseSync(p, { readOnly: true });
const rows = db
  .prepare("select kind, entity_id, ord, json from config order by kind, ord")
  .all();
const byKind = {};
for (const r of rows) (byKind[r.kind] ??= []).push(r);
console.log("kinds =", JSON.stringify(Object.fromEntries(Object.entries(byKind).map(([k, v]) => [k, v.length]))));

const parse = (r) => {
  try {
    return JSON.parse(String(r.json));
  } catch {
    return {};
  }
};

console.log("\n--- subscription 表 ---");
try {
  console.log(JSON.stringify(db.prepare("select id, name, format, enabled, node_count, group_count from subscription").all()));
} catch (e) {
  console.log("ERR", e.message);
}

console.log("\n--- net 配置 ---");
const net = byKind["section"]?.find((r) => String(r.entity_id) === "net");
if (net) console.log(JSON.stringify(parse(net)).slice(0, 600));

console.log("\n--- lanes（doc.lanes） ---");
for (const r of byKind["lane"] ?? []) {
  const j = parse(r);
  console.log(`  ${r.entity_id}: kind=${j.kind} name=${JSON.stringify(j.name)} enabled=${j.enabled} node=${JSON.stringify(j.node ?? null)} group=${JSON.stringify(j.group ?? null)}`);
}

console.log("\n--- providers / credentials ---");
for (const r of byKind["provider"] ?? []) console.log(`  provider ${r.entity_id} lane=${JSON.stringify(parse(r).lane ?? null)}`);
for (const r of byKind["credential"] ?? []) console.log(`  credential ${r.entity_id} provider=${parse(r).provider} lane=${JSON.stringify(parse(r).lane ?? null)}`);

console.log("\n--- channels（线路）最近的几条 ---");
for (const r of (byKind["channel"] ?? []).slice(0, 12)) {
  const j = parse(r);
  console.log(`  ${r.entity_id}: provider=${j.provider} lane=${JSON.stringify(j.lane ?? null)} upstream=${j.upstream_model}`);
}

console.log("\n--- models 的 lines ---");
for (const r of byKind["model"] ?? []) {
  const j = parse(r);
  console.log(`  ${r.entity_id}: lines=${JSON.stringify(j.lines ?? [])}`);
}

console.log("\n--- provider_model_catalog（扫描记录） ---");
try {
  console.log(JSON.stringify(db.prepare("select provider_id, upstream_model from provider_model_catalog limit 10").all()));
} catch (e) {
  console.log("ERR", e.message);
}

console.log("\n--- config(kind=subscription) 原文 ---");
for (const r of byKind["subscription"] ?? []) console.log(`  ${r.entity_id}: ${String(r.json).slice(0, 500)}`);

console.log("\n--- subscription_version 表 ---");
try {
  const cols = db.prepare("select name from pragma_table_info('subscription_version')").all();
  console.log("cols =", JSON.stringify(cols.map((c) => c.name)));
  const vs = db.prepare("select * from subscription_version order by rowid desc limit 3").all();
  console.log(JSON.stringify(vs).slice(0, 800));
} catch (e) {
  console.log("ERR", e.message);
}

console.log("\n--- net 里与订阅/内核有关的 section ---");
for (const r of (byKind["section"] ?? []).filter((x) => ["net", "system_integration"].includes(String(x.entity_id)))) {
  console.log(`  ${r.entity_id}: ${String(r.json).slice(0, 700)}`);
}
db.close();
