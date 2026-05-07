#!/usr/bin/env node

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const srcDir = path.join(__dirname, "..", "src");
const componentsDir = path.join(srcDir, "components");

// Ensure components directory exists
fs.mkdirSync(componentsDir, { recursive: true });

function extractReactComponent(filePath) {
  const content = fs.readFileSync(filePath, "utf8");
  const lines = content.split("\n");
  
  // Find the frontmatter end
  const frontmatterEnd = lines.findIndex(line => line.trim() === "---");
  if (frontmatterEnd === -1) return false;
  
  // Check if there's React code after the frontmatter
  const afterFrontmatter = lines.slice(frontmatterEnd + 1).join("\n");
  
  // Look for React patterns
  const hasReactCode = afterFrontmatter.includes("useState") || 
                       afterFrontmatter.includes("useEffect") ||
                       afterFrontmatter.includes("export default function") ||
                       afterFrontmatter.includes("export function");
  
  if (!hasReactCode) return false;
  
  // Extract the React code (everything after frontmatter until the Layout tag)
  const layoutMatch = afterFrontmatter.match(/<Layout>/);
  if (!layoutMatch) return false;
  
  const reactCode = afterFrontmatter.substring(0, layoutMatch.index).trim();
  
  // Generate a component name based on the file path
  const relativePath = path.relative(srcDir, filePath);
  const componentName = relativePath
    .replace(/\//g, "-")
    .replace(/\.astro$/, "")
    .replace(/-page$/, "PageClient")
    .replace(/-/, "")
    .split("-")
    .map(part => part.charAt(0).toUpperCase() + part.slice(1))
    .join("");
  
  // Write the React component to a .jsx file
  const componentPath = path.join(componentsDir, `${componentName}.jsx`);
  fs.writeFileSync(componentPath, reactCode, "utf8");
  
  // Update the .astro file to use the component
  const newContent = `---
import Layout from '@/layouts/Layout.astro';
import ${componentName} from '@/components/${componentName}';
---
<Layout>
  <${componentName} />
</Layout>`;
  
  fs.writeFileSync(filePath, newContent, "utf8");
  console.log(`Extracted ${componentName} from ${relativePath}`);
  return true;
}

function findAndExtract(dir) {
  const entries = fs.readdirSync(dir, { withFileTypes: true });
  
  for (const entry of entries) {
    const fullPath = path.join(dir, entry.name);
    
    if (entry.isDirectory()) {
      findAndExtract(fullPath);
    } else if (entry.name.endsWith(".astro")) {
      extractReactComponent(fullPath);
    }
  }
}

findAndExtract(srcDir);
console.log("Done extracting React components");
