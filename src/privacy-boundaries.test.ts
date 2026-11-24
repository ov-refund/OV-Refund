import adapter from '../src-tauri/src/ns_adapters.rs?raw';
import hook from '../src-tauri/installer-hooks.nsh?raw';
import config from '../src-tauri/tauri.conf.json?raw';
import bridge from '../src-tauri/src/ns_bridge.rs?raw';
import releaseScript from '../tools/release.ps1?raw';
import unsignedConfig from '../tools/unsigned-bundle.json?raw';
import { describe, expect, it } from 'vitest';

const sources: Record<string, string> = { 'src-tauri/src/ns_adapters.rs': adapter, 'src-tauri/installer-hooks.nsh': hook, 'src-tauri/tauri.conf.json': config, 'src-tauri/src/ns_bridge.rs': bridge, 'tools/release.ps1': releaseScript, 'tools/unsigned-bundle.json': unsignedConfig };
const source = (path: string) => sources[path];

describe('privacy boundaries (offline)', () => {
  it('types bank details only into the intended field on the top-level HTTPS NS page', () => {
    const rust = source('src-tauri/src/ns_adapters.rs');
    const body = rust.slice(rust.indexOf('fn bank_focus_script('));
    const literal = body.match(/format!\("((?:\\.|[^"\\])*)"\)/)![1];
    const script = JSON.parse(`"${literal}"`).replaceAll('{{', '{').replaceAll('}}', '}').replaceAll('{field_expr}', "window.__ovq('#iban')");
    const check = new Function('window', 'location', 'document', `return ${script}`);
    const field = {};
    const page = (options: { top?: boolean; protocol?: string; host?: string; active?: unknown; field?: unknown } = {}) => {
      const window: Record<string, unknown> = { __ovq: () => 'field' in options ? options.field : field };
      window.top = options.top === false ? {} : window;
      return check(window, { protocol: options.protocol ?? 'https:', hostname: options.host ?? 'www.ns.nl' }, { activeElement: 'active' in options ? options.active : field });
    };
    expect(page()).toBe(true);
    expect(page({ active: { shadowRoot: { activeElement: field } } })).toBe(true);
    for (const options of [{ top: false }, { protocol: 'http:' }, { host: 'www.ns.nl.evil.example' }, { host: 'login.ns.nl' }, { active: {} }, { active: null }, { field: null }]) {
      expect(page(options)).toBe(false);
    }
  });

  it('always clears the uninstall profile after forgetting login, and limits taskkill to the current user', () => {
    const hook = source('src-tauri/installer-hooks.nsh');
    const optional = hook.slice(hook.indexOf('${If} $DeleteAppDataCheckboxState'), hook.indexOf('${EndIf}', hook.indexOf('${If} $DeleteAppDataCheckboxState')));
    expect(optional).toContain('RmDir /r "$LOCALAPPDATA\\OVRefund"');
    expect(optional).not.toContain('EBWebView');
    expect(hook.indexOf('remove-schedule')).toBeLessThan(hook.indexOf('nsExec::Exec'));
    expect(hook.indexOf('nsExec::Exec')).toBeLessThan(hook.indexOf('forget-login\''));
    expect(hook.indexOf('forget-login\'')).toBeLessThan(hook.indexOf('RmDir /r "$LOCALAPPDATA\\nl.ovrefund.app\\EBWebView"'));
    expect(hook).toContain('UserInfo::GetName');
    expect(hook).toContain('/FI "USERNAME eq $0"');
    expect(JSON.parse(source('src-tauri/tauri.conf.json')).identifier).toBe('nl.ovrefund.app');
    expect(source('src-tauri/src/ns_bridge.rs')).toContain('COREWEBVIEW2_BROWSING_DATA_KINDS_ALL_PROFILE');
  });

  it('builds without signing secrets and signs separately with an explicit environment', () => {
    const release = source('tools/release.ps1');
    const build = release.indexOf('npx --offline tauri build');
    expect(release.indexOf('Remove-Item Env:TAURI_SIGNING_PRIVATE_KEY')).toBeLessThan(build);
    expect(release.indexOf("'updater.key') -Raw")).toBeGreaterThan(build);
    expect(release).toContain('$sign.EnvironmentVariables.Clear()');
    expect(release).toContain('signer sign --app-version');
    expect(JSON.parse(source('tools/unsigned-bundle.json')).bundle.createUpdaterArtifacts).toBe(false);
  });
});


it('hit-tests the exact target after scrolling and rejects covered or moving elements', () => {
  const source = bridge.slice(bridge.indexOf('pub fn click('), bridge.indexOf('pub fn navigate('));
  expect(source.indexOf('"mouseMoved"')).toBeLessThan(source.indexOf('after_hover().map_err'));
  expect(source.indexOf('after_hover().map_err')).toBeLessThan(source.indexOf('let hit = self.eval'));
  expect(source.indexOf('let hit = self.eval')).toBeLessThan(source.indexOf('before_press().map_err'));
  expect(source.indexOf('before_press().map_err')).toBeLessThan(source.indexOf('"mousePressed"'));
  const submit = adapter.indexOf('b.click_after(&button.expr');
  expect(submit).toBeGreaterThanOrEqual(0);
  const hover = adapter.slice(submit, adapter.indexOf('}, &|| {', submit));
  for (const check of ['form_check(b)', 'current_claim_route', 'final_form_values', 'submitOk']) expect(hover).toContain(check);
  const literal = source.match(/let hit = self.eval\(&format!\("((?:\\.|[^"\\])*)"/s)![1];
  const script = JSON.parse(`"${literal}"`).replaceAll('{{', '{').replaceAll('}}', '}').replaceAll('{element_expr}', 'target').replaceAll('{x}', '50').replaceAll('{y}', '20');
  const check = new Function('target', 'document', `return ${script}`);
  const target = { getBoundingClientRect: () => ({ x: 40, y: 10, width: 20, height: 20 }), contains: (el: unknown) => el === child };
  // NS draws controls inside shadow roots: the hit test walks parentNode and shadow host upwards.
  const child: Record<string, unknown> = { parentNode: target };
  const shadowChild = { parentNode: { host: target } };
  expect(check(target, { elementFromPoint: () => target })).toBe(true);
  expect(check(target, { elementFromPoint: () => child })).toBe(true);
  expect(check(target, { elementFromPoint: () => shadowChild })).toBe(true);
  // NS slots a button's text into the component's shadow button: the hit is on the slotted span.
  expect(check(target, { elementFromPoint: () => ({ parentNode: {}, assignedSlot: { parentNode: target } }) })).toBe(true);
  expect(check(target, { elementFromPoint: () => ({}) })).toBe(false);
  expect(check(target, { elementFromPoint: () => null })).toBe(false);
  expect(check({ ...target, getBoundingClientRect: () => ({ x: 80, y: 10, width: 20, height: 20 }) }, { elementFromPoint: () => target })).toBe(false);
  const host = { shadowRoot: { elementFromPoint: () => child } };
  expect(check(target, { elementFromPoint: () => host })).toBe(true);
  const covered = { shadowRoot: { elementFromPoint: () => ({}) } };
  expect(check(target, { elementFromPoint: () => covered })).toBe(false);
});
