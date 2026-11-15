// Bundles entry.js and its CodeMirror dependencies into one minified file and writes it,
// with the licence texts, to crates/rustmail-web/static/vendor/codemirror/6/.
//
//   cd scripts/vendor-codemirror && npm ci && npm run build
//
// The output is committed; this is only needed to rebuild or upgrade it.
import { build } from 'esbuild';
import { copyFileSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const out = path.resolve(here, '../../crates/rustmail-web/static/vendor/codemirror/6');
mkdirSync(out, { recursive: true });

// CodeMirror sets some styles with `element.style.cssText = "..."`. A Content-Security-Policy
// with `style-src 'self'` blocks that (setting individual properties through the CSSOM is
// allowed), and RustMail does not relax the policy. This rewrites those assignments to set
// the same properties one by one. It fails the build if upstream changes, so a new version
// cannot silently bring the problem back.
const EXPECTED_CSSTEXT_WRITES = 6;
const helper = `function rmCss(style, text, append) {
  if (!append) while (style.length) style.removeProperty(style.item(0));
  for (const part of String(text).split(';')) {
    const i = part.indexOf(':');
    if (i > 0) style.setProperty(part.slice(0, i).trim(), part.slice(i + 1).trim());
  }
}
`;
const cspSafeStyles = {
  name: 'csp-safe-styles',
  setup(b) {
    b.onLoad({ filter: /@codemirror[\\/]view[\\/]dist[\\/]index\.js$/ }, (args) => {
      let writes = 0;
      const source = readFileSync(args.path, 'utf8').replace(
        /([\w.]+)\.style\.cssText\s*(\+?)=\s*("(?:[^"\\]|\\.)*"|[\w.[\]]+);/g,
        (_, target, plus, value) => {
          writes++;
          return `rmCss(${target}.style, ${value}, ${plus === '+'});`;
        },
      );
      if (writes !== EXPECTED_CSSTEXT_WRITES) {
        throw new Error(`expected ${EXPECTED_CSSTEXT_WRITES} cssText writes in @codemirror/view, found ${writes}: review the CSP shim in build.mjs`);
      }
      return { contents: helper + source, loader: 'js' };
    });
  },
};

await build({
  plugins: [cspSafeStyles],
  entryPoints: [path.join(here, 'entry.js')],
  bundle: true,
  minify: true,
  format: 'iife',
  target: ['es2020'],
  legalComments: 'none',
  outfile: path.join(out, 'sieve-editor.js'),
});

// MIT requires the notice to travel with copies: one file listing each package's licence.
const packages = ['@codemirror/commands', '@codemirror/language', '@codemirror/legacy-modes', '@codemirror/state', '@codemirror/view', '@lezer/common', '@lezer/highlight', '@lezer/lr', 'style-mod', 'w3c-keyname', 'crelt', '@marijn/find-cluster-break'];
let notice = 'The editor bundle (sieve-editor.js) contains these MIT-licensed packages.\n\n';
for (const name of packages) {
  const dir = path.join(here, 'node_modules', name);
  let version = '?';
  let text = '';
  try {
    version = JSON.parse(readFileSync(path.join(dir, 'package.json'), 'utf8')).version;
    text = readFileSync(path.join(dir, 'LICENSE'), 'utf8').trim();
  } catch {
    continue; // not a dependency of this build
  }
  notice += `=== ${name} ${version} ===\n${text}\n\n`;
}
writeFileSync(path.join(out, 'LICENSES.txt'), notice);

const sha = createHash('sha384').update(readFileSync(path.join(out, 'sieve-editor.js'))).digest('base64');
console.log(`wrote ${path.join(out, 'sieve-editor.js')}\nsha384-${sha}`);
