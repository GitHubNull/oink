// @vitest-environment jsdom
import { afterEach, beforeAll, expect, it, vi } from 'vitest';
import { cleanup, render } from '@testing-library/react';
import { SettingsPreview } from './SettingsPreview';
beforeAll(() => {
  Object.defineProperty(window, 'matchMedia', { value: () => ({ matches: true, addEventListener() {}, removeEventListener() {} }) });
  vi.stubGlobal('ResizeObserver', class { observe() {} unobserve() {} disconnect() {} });
  HTMLElement.prototype.scrollTo = () => {};
  HTMLElement.prototype.scrollIntoView = () => {};
});
afterEach(() => { cleanup(); window.history.replaceState({}, '', '/'); });
it('renders the Linux branch without macOS-only or unimplemented rows', () => {
  window.history.replaceState({}, '', '/?fixture=settings&platform=linux&lang=zh-Hans');
  const { container } = render(<SettingsPreview/>);
  expect(container.querySelector('[data-section="permissions"]')).toBeNull();
  expect(container.querySelector('[data-item="removeDsStore"]')).toBeNull();
  expect(container.querySelector('[data-item="virtualMicrophone"]')).toBeNull();
  expect(container.querySelector('[data-item="showInMenuBar"]')?.textContent).toContain('系统托盘的图标');
});
