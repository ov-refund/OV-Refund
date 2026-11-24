// Print the *shape* of a captured JSON body with personal values masked, so
// structure can be documented without copying personal data into reports.
// Usage: node tools/investigate/shape.mjs <file.json> [maxArrayItems=2]
import fs from 'node:fs';

const [file, maxItems = '2'] = process.argv.slice(2);
const SENSITIVE_KEY = /(name|naam|email|iban|phone|telefoon|address|adres|street|straat|postal|zip|birth|geboorte|card|kaart|engraved|number|nummer|token|cookie|initial|ssn|bsn|id$)/i;

function mask(key, v) {
  if (typeof v !== 'string') return v;
  if (SENSITIVE_KEY.test(key)) return `<str:${v.length}>`;
  return v.replace(/\d{8,}/g, '<NUM>').replace(/[\w.+-]+@[\w.-]+/g, '<EMAIL>').slice(0, 60);
}

function walk(v, key = '', depth = 0) {
  if (Array.isArray(v)) return [`<array len=${v.length}>`, ...v.slice(0, Number(maxItems)).map((x) => walk(x, key, depth + 1))];
  if (v && typeof v === 'object') return Object.fromEntries(Object.entries(v).map(([k, x]) => [k, walk(x, k, depth + 1)]));
  return mask(key, v);
}

console.log(JSON.stringify(walk(JSON.parse(fs.readFileSync(file, 'utf8'))), null, 2));
