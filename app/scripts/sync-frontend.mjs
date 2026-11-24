import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  statSync,
} from 'node:fs';
import { dirname, join, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const appRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const source = join(appRoot, 'frontend', 'dist');
const destination = join(appRoot, 'src', 'web');
const indexPath = join(source, 'index.html');

if (!existsSync(indexPath)) {
  throw new Error('Build the frontend before embedding it: npm run build');
}

const index = readFileSync(indexPath, 'utf8');
const referencedAssets = [...index.matchAll(/(?:src|href)="[/](assets[/][^"]+)"/g)].map((match) => match[1]);

if (referencedAssets.length === 0) {
  throw new Error('Frontend build has no asset references; refusing to embed an incomplete build');
}

for (const asset of referencedAssets) {
  const fullPath = resolve(source, asset);
  if (!fullPath.startsWith(resolve(source) + sep) || !existsSync(fullPath) || !statSync(fullPath).isFile()) {
    throw new Error(`Unexpected or missing asset: ${asset}`);
  }
  mkdirSync(dirname(join(destination, asset)), { recursive: true });
  copyFileSync(fullPath, join(destination, asset));
}

mkdirSync(destination, { recursive: true });
copyFileSync(indexPath, join(destination, 'index.html'));

const unexpected = readdirSync(join(source, 'assets')).filter(
  (name) => !referencedAssets.includes(`assets/${name}`),
);
if (unexpected.length > 0) {
  throw new Error(`Generated assets not referenced by index.html: ${unexpected.join(', ')}. Review before embedding.`);
}

process.stdout.write(`Embedded frontend index and ${referencedAssets.length} assets in ${relative(appRoot, destination)}\n`);
