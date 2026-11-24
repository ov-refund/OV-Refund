declare module '*?raw' {
  const content: string;
  export default content;
}

// Narrow file-reading type for offline source/CSS regression tests.
declare module 'node:fs' {
  export function readFileSync(path: URL | string, encoding: 'utf8'): string;
  export function readdirSync(path: string, options: { withFileTypes: true }): { name: string; isDirectory(): boolean }[];
  export function existsSync(path: string): boolean;
  export function statSync(path: string): { isDirectory(): boolean };
}

declare module 'node:path' {
  export function resolve(...paths: string[]): string;
  export function relative(from: string, to: string): string;
}

declare module 'node:url' {
  export function fileURLToPath(url: URL | string): string;
}
