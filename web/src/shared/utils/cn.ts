// Utility function to merge class names
// Handles conditional classes and removes duplicates

import type { ClassNameValue } from "@/types";

export function cn(...classes: ClassNameValue[]): string {
  return classes
    .filter(Boolean)
    .join(" ")
    .replace(/\s+/g, " ")
    .trim();
}
