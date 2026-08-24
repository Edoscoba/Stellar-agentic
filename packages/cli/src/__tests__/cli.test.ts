import { describe, it, expect, vi } from 'vitest';
import { execFileSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';

// The package builds to CommonJS, so `import.meta.url` is unavailable here.
// Vitest runs with cwd set to the package root, which is what we anchor on.
const pkgRoot = process.cwd();
const entry = resolve(pkgRoot, 'src/index.ts');
const built = resolve(pkgRoot, 'dist/index.js');

/**
 * The CLI is still a stub (`console.log("StellarAgent CLI coming soon!")`),
 * so there is no command surface to test yet. What these tests protect is the
 * packaging contract — the `bin` entry resolves, the shebang is intact, and
 * the built artifact actually executes — which is exactly what breaks
 * silently when the build config is changed.
 */
describe('@stellaragent/cli packaging', () => {
  const pkg = JSON.parse(readFileSync(resolve(pkgRoot, 'package.json'), 'utf8'));

  it('declares a `stellaragent` bin entry', () => {
    expect(pkg.bin).toEqual({ stellaragent: 'dist/index.js' });
  });

  it('has a shebang so the bin is directly executable', () => {
    expect(readFileSync(entry, 'utf8').startsWith('#!/usr/bin/env node')).toBe(true);
  });

  it('runs and prints to stdout', async () => {
    const log = vi.spyOn(console, 'log').mockImplementation(() => {});
    try {
      vi.resetModules();
      await import('../index.js');
      expect(log).toHaveBeenCalledWith('StellarAgent CLI coming soon!');
    } finally {
      log.mockRestore();
    }
  });

  // `turbo run test` builds first (test dependsOn build), so dist/ exists in
  // CI. Locally it may not, hence the guard rather than a hard failure.
  it.runIf(existsSync(built))('the built bin executes', () => {
    const out = execFileSync(process.execPath, [built], { encoding: 'utf8' });
    expect(out.trim()).toBe('StellarAgent CLI coming soon!');
  });

  it('the bin path in package.json points at the build output', () => {
    expect(pkg.bin.stellaragent).toBe('dist/index.js');
    expect(pkg.scripts.build).toContain('src/index.ts');
  });
});
