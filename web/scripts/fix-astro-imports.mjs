#!/usr/bin/env node

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const srcDir = path.join(__dirname, "..", "src");

function fixAstroFile(filePath) {
  const content = fs.readFileSync(filePath, "utf8");
  const lines = content.split("\n");
  
  // Find the frontmatter end
  const frontmatterEnd = lines.findIndex(line => line.trim() === "---");
  if (frontmatterEnd === -1) return;
  
  // Extract the frontmatter
  const frontmatter = lines.slice(0, frontmatterEnd + 1).join("\n");
  
  // Extract the import statement from the old content
  const importMatch = content.match(/import\s+(\w+)\s+from\s+["']\.\/([^"']+)["']/);
  if (!importMatch) return;
  
  const componentName = importMatch[1];
  const componentPath = importMatch[2];
  
  // Build the new content
  const newContent = `${frontmatter}
import ${componentName} from '@/components/${componentPath.replace('.jsx', '')}';
---
<Layout>
  <${componentName} />
</Layout>`;
  
  fs.writeFileSync(filePath, newContent, "utf8");
  console.log(`Fixed ${filePath}`);
}

function findAndFixAstroFiles(dir) {
  const entries = fs.readdirSync(dir, { withFileTypes: true });
  
  for (const entry of entries) {
    const fullPath = path.join(dir, entry.name);
    
    if (entry.isDirectory()) {
      findAndFixAstroFiles(fullPath);
    } else if (entry.name.endsWith(".astro")) {
      fixAstroFile(fullPath);
    }
  }
}

findAndFixAstroFiles(srcDir);
console.log("Done fixing Astro files");
