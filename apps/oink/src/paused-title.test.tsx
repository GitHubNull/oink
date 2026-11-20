// @vitest-environment jsdom
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeAll, describe, expect, it, vi } from 'vitest';
import { App } from './App';
import { shell } from './api/client';
import { agent } from './api/store';
import type { DeviceCardsView, LocalPauseCard } from './api/cards';
import { setLanguage } from './i18n';

vi.mock('./api/store', async original => ({ ...await original<typeof import('./api/store')>(), start: vi.fn() }));
vi.mock('./api/transport', async original => ({
  ...await original<typeof import('./api/transport')>(),
  transport: {
    call: async ({ method }: { method: string }) => method === 'group' ? { members: [] } : [],
    shell: async () => null,
    onEvent: async () => () => {},
  },
}));
beforeAll(() => {
  vi.stubGlobal('ResizeObserver', class { observe() {} unobserve() {} disconnect() {} });
  Object.defineProperty(window, 'matchMedia', { value: () => ({ matches: false, addEventListener() {}, removeEventListener() {} }) });
});
afterEach(() => { cleanup(); agent.reset(); localStorage.clear(); vi.restoreAllMocks(); });

function cards(paused: boolean, reason: LocalPauseCard['reason']): DeviceCardsView {
  return { schema: 1, stream: 'test', revision: '1', group: null, generatedAtMs: 0, exitVolume: null, emergencyDelivery: null,
    cards: [{ device: '1'.repeat(64), name: 'This device', os: 'macos', logo: 'macos', isSelf: true, section: 'this_device', connection: { kind: 'self' }, lastSeenMs: null, status: null,
      pause: { paused, reason, revision: '1', canResume: true, action: { kind: 'unavailable', reason: 'state_unknown' } } }],
  };
}

for (const platform of ['macos', 'windows', 'linux'] as const) {
  for (const [language, label] of [['zh-Hans', '共享已暂停'], ['en', 'Sharing paused']] as const) {
    describe(`${platform} paused title in ${language}`, () => {
      it.each(['local_user', 'remote_user', 'emergency', 'input_backlog'] as const)('shows a decorative pause glyph for %s, and hides the status on resume', reason => {
        setLanguage(language);
        agent.adopt('boot', { platform, version: '0.1.0-beta.1', build: null, backend: 'agent' });
        agent.adopt('cards', cards(true, reason));
        const { container } = render(<App/>);
        const status = screen.getByRole('status');
        expect(status.textContent).toBe(label);
        expect(status.closest('.win')?.getAttribute('data-platform')).toBe(platform === 'macos' ? 'mac' : 'win');
        const icon = status.querySelector('svg')!;
        expect(icon.getAttribute('aria-hidden')).toBe('true');
        expect(icon.getAttribute('focusable')).toBe('false');
        expect(icon.querySelector('circle')).not.toBeNull();
        expect(icon.querySelector('path')).not.toBeNull();
        expect(status.querySelector('.win__paused')?.firstChild).toBe(icon);
        expect(status.hasAttribute('data-shown')).toBe(true);
        act(() => agent.adopt('cards', cards(false, 'none')));
        expect(screen.queryByRole('status')).toBeNull();
        expect(container.querySelector('.win__title-center')?.getAttribute('aria-hidden')).toBe('true');
      });
    });
  }
}

describe('tray visibility setting', () => {
  const local = { notification_level: 'default', ui_language: 'auto', theme_mode: 'system', theme_accent: { light: '#2E7BFF', dark: '#5599FF' },
    show_status_icon: true, confirm_quit: true, paste_as_file: true, log_level: 'info', log_max_mb: 1024 } as const;
  it('applies the saved state through the shell on Linux but leaves macOS to the native shell', async () => {
    const setStatusIcon = vi.spyOn(shell, 'setStatusIcon').mockResolvedValue();
    agent.adopt('boot', { platform: 'linux', version: '0.1.0-beta.1', build: null, backend: 'agent' });
    agent.adopt('localPreferences', local);
    const linux = render(<App/>);
    await waitFor(() => expect(setStatusIcon).toHaveBeenCalledWith(true));
    linux.unmount();setStatusIcon.mockClear();
    agent.adopt('boot', { platform: 'macos', version: '0.1.0-beta.1', build: null, backend: 'agent' });
    render(<App/>);
    await new Promise(resolve => setTimeout(resolve, 0));
    expect(setStatusIcon).not.toHaveBeenCalled();
  });
});
