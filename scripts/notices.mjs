// Writes THIRD-PARTY-NOTICES.md: Rust crates through cargo-about, plus the frontend packages
// that end up in the shipped bundle. Rerun whenever dependencies change.
import { execFileSync } from 'node:child_process';
import { appendFileSync, readFileSync, readdirSync } from 'node:fs';
import { join } from 'node:path';

const OUTPUT = 'THIRD-PARTY-NOTICES.md';

// Only packages whose code or assets are compiled into dist/.
const FRONTEND = ['@fontsource/saira-semi-condensed', '@phosphor-icons/core', '@tauri-apps/api'];

execFileSync('cargo', ['about', 'generate', 'about.hbs', '--offline', '--output-file', OUTPUT], {
  stdio: 'inherit',
});

appendFileSync(OUTPUT, '## Frontend packages\n\n');
for (const name of FRONTEND) {
  const dir = join('node_modules', name);
  const licences = readdirSync(dir).filter((file) => /^licen[sc]e/i.test(file) && !file.endsWith('.spdx'));
  if (licences.length === 0) throw new Error(`No licence file in ${dir}`);
  for (const file of licences) {
    const text = readFileSync(join(dir, file), 'utf8').trim();
    appendFileSync(OUTPUT, `### ${name}\n\n\`\`\`\n${text}\n\`\`\`\n\n`);
  }
}
