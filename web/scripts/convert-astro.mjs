import fs from 'fs';
import path from 'path';
import { fileURLToPath } from 'url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const pagesDir = path.join(__dirname, '../src/pages');

function convertPageToAstro(filePath) {
  const content = fs.readFileSync(filePath, 'utf-8');
  
  // Remove Next.js specific imports
  let newContent = content
    .replace(/import \{ redirect \} from "next\/navigation";/g, '')
    .replace(/import \{ notFound \} from "next\/navigation";/g, '')
    .replace(/export const metadata = \{[^}]+\};?/g, '')
    .replace(/export const viewport = \{[^}]+\};?/g, '')
    .replace(/export default function/g, 'const Page =')
    .replace(/export default async function/g, 'const Page = async')
    .replace(/export default/g, 'const Page =');
  
  // Add Astro frontmatter
  const frontmatter = `---
import Layout from '@/layouts/Layout.astro';
---
`;
  
  // Wrap in Layout component
  newContent = frontmatter + newContent
    .replace(/const Page = \(([^)]+)\) => \{/, 'const Page = ($1) => {')
    .replace(/\n\}/, '\n<Layout>\n  <Page />\n</Layout>');
  
  fs.writeFileSync(filePath, newContent);
  console.log(`Converted ${filePath}`);
}

// Find all .astro files and convert them
function findAndConvert(dir) {
  const files = fs.readdirSync(dir, { withFileTypes: true });
  
  for (const file of files) {
    const fullPath = path.join(dir, file.name);
    
    if (file.isDirectory()) {
      findAndConvert(fullPath);
    } else if (file.name.endsWith('.astro')) {
      convertPageToAstro(fullPath);
    }
  }
}

findAndConvert(pagesDir);
console.log('Conversion complete!');
