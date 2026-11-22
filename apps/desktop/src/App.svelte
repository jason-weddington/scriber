<script lang="ts">
  // The dictation screen: New, Record, Pause, Resume, Done; the transcript
  // appears in an editable box and is already on the clipboard. Every rule
  // lives in the core; this file shows its snapshot and sends the user's
  // actions. Patterns: docs/tauri-design-notes.md. The library drawer on the
  // left lists past dictations; selecting one shows it in place of the
  // recorder until New.
  import { onMount, tick } from 'svelte';

  import {
    act,
    appVersion,
    configureWebConnection,
    dictationState,
    errorText,
    INITIAL_WEB_UI_STATE,
    inputDevices,
    isWebBackend,
    libraryCopy,
    libraryGet,
    libraryList,
    learnedVocabularyOpen,
    observeWebConnectDecision,
    onDictationChanged,
    ollamaModels,
    onFullscreenChanged,
    onLevel,
    onWebRebind,
    onWebUiStateChanged,
    pickModelFolder,
    pickVocabularyFile,
    projectWebSnapshot,
    retryWebConnection,
    settingsGet,
    settingsSet,
    vocabularyOpen,
    WebCopyError,
    windowFullscreen,
    type WebAttemptFailureCode,
    type WebConnectionConfig,
    type WebUiState,
  } from './lib/bridge';
  import {
    activeMicrophone,
    allowed,
    bars,
    barCount,
    controlsFor,
    DEFAULT_PAUSE_SHORTCUT,
    DEFAULT_TOGGLE_SHORTCUT,
    downloadText,
    formatElapsed,
    percent,
    pushLevel,
    shortcut,
    opensSettings,
    SETTINGS_SHORTCUT,
    LIBRARY_WIDTH_DEFAULT,
    SHORTCUTS,
    statusAlert,
    statusLine,
  } from './lib/format';
  import ConnectPanel from './lib/ConnectPanel.svelte';
  import CopyButton from './lib/CopyButton.svelte';
  import LibraryDrawer from './lib/LibraryDrawer.svelte';
  import MicrophoneMenu from './lib/MicrophoneMenu.svelte';
  import SettingsPanel from './lib/SettingsPanel.svelte';
  import type { Action, InputDevices, LibraryEntry, Settings, Snapshot } from './lib/types';

  let snapshot = $state<Snapshot | null>(null);
  let loadError = $state<string | null>(null);
  let actionError = $state<string | null>(null);
  let busy = $state(false);
  let draft = $state('');
  let version = $state('');
  // While recording, the running segment's time is counted here: the core
  // only knows the length of segments that are closed.
  let recordingSince = $state<number | null>(null);
  let now = $state(Date.now());
  // The input levels the waveform draws, oldest first. Kept while paused,
  // cleared once the session leaves recording and paused.
  let levels = $state<number[]>([]);
  // The waveform's width in px, which sets how many bars fit.
  let waveWidth = $state(0);
  // Set when a Space keydown ran a shortcut, so its keyup cannot also click
  // a focused button.
  let swallowSpaceUp = false;
  // Whether the focused element got its focus from a click rather than the
  // keyboard. (Chromium's :focus-visible cannot tell: any key press makes the
  // focused element match it.)
  let pointerDown = false;
  let focusFromPointer = false;
  // Where Space and Return type rather than act (a switch toggles on Space).
  const TEXT_FIELDS =
    'textarea, select, input:not([type=button], [type=checkbox], [type=radio], [type=submit], [type=reset]), input[role=switch]';

  // The settings, as last read or chosen; the defaults until they load.
  let settings = $state<Settings>({ summaries: true, libraryOpen: true, vocabulary: '', cleanup: false, summaryModel: '', cleanupModel: '', libraryWidth: LIBRARY_WIDTH_DEFAULT, inputDevice: '', silenceSplitMs: 0, keepSegments: false, learnedVocabulary: '', modelFolder: '', toggleShortcut: DEFAULT_TOGGLE_SHORTCUT, pauseShortcut: DEFAULT_PAUSE_SHORTCUT, copySound: true });
  let settingsOpen = $state(false);
  let settingsPanel = $state<{ closeDialog: () => void }>();
  let settingsInitialFocus = $state<'default' | 'connect'>('default');
  let settingsButton = $state<HTMLButtonElement>();
  let settingsFromPointer = false;
  let settingsError = $state<string | null>(null);
  let settingsLoaded = $state(!isWebBackend());
  // The installed Ollama models, listed each time Settings opens; null while
  // the listing is in flight.
  let models = $state<string[] | null>(null);
  // Whether the last listing could not reach Ollama, and why.
  let ollamaDown = $state(false);
  let ollamaReason = $state('');
  // Which listing is the latest, so a slow earlier answer cannot overwrite it.
  let modelListing = 0;
  // The input devices as last listed (null before the first answer), why the
  // latest listing failed, and which listing is the latest.
  let inputs = $state<InputDevices | null>(null);
  let inputsError = $state<string | null>(null);
  let inputListing = 0;
  // A failure to save the drawer's open state or width, shown in the band.
  let toggleError = $state<string | null>(null);
  let fullscreen = $state(false);
  let entries = $state<LibraryEntry[]>([]);
  let libraryError = $state<string | null>(null);
  // The highest libraryRevision the list has been fetched for.
  let listedRevision = -1;
  // Which fetch is the latest, so a slow earlier answer cannot overwrite it.
  let listing = 0;
  // The library entry shown in place of the recorder, if any.
  let entry = $state<{ id: string; title: string; transcript: string | null } | null>(null);
  let entryError = $state<string | null>(null);
  let entryCopied = $state(false);

  const webBackend = isWebBackend();
  let webUi = $state<WebUiState>(INITIAL_WEB_UI_STATE);
  let sessionGeneration = 0;
  let entryRequest = 0;
  let resetSnapshotBaseline = false;
  let activeOrigin: string | null = null;
  let lastHandledAttemptKey = '';

  type ConnectOrigin = 'manual_settings' | 'auto' | 'manual_recovery';
  let connectOpen = $state(false);
  let connectOrigin = $state<ConnectOrigin | null>(null);
  let connectSubmitted = $state(false);
  let connectFeedback = $state<string | null>(null);
  let recoveryConnectButton = $state<HTMLButtonElement>();
  let pendingRecorderFocus = false;

  const webNeedsRecovery = $derived(
    webBackend &&
      webUi.connection.status === 'failed' &&
      (webUi.connection.failureCode === 'auth_4401' ||
        (!webUi.connection.priorAuth && webUi.connection.failureCode !== 'disconnect') ||
        webUi.connection.reason === 'configure' ||
        webUi.connection.reason === 'explicit_retry'),
  );
  const webRecoveryMessage = $derived(
    webUi.connection.status === 'failed' && webUi.connection.failureCode === 'auth_4401'
      ? 'Authentication refused.'
      : 'Could not connect to the Scriber server.',
  );
  const connectConnecting = $derived(
    connectSubmitted && webUi.connection.status === 'connecting',
  );
  const connectCanRetry = $derived(webUi.connection.status !== 'authenticated');

  const elapsed = $derived(
    snapshot === null ? 0 : snapshot.recordedMs + (recordingSince === null ? 0 : Math.max(0, now - recordingSince)),
  );
  const metering = $derived(snapshot?.phase === 'recording' || snapshot?.phase === 'paused');
  const displaySnapshot = $derived(
    snapshot === null ? null : webBackend ? projectWebSnapshot(snapshot, webUi) : snapshot,
  );
  const line = $derived(displaySnapshot === null ? '' : statusLine(displaySnapshot));
  const alert = $derived(snapshot === null ? null : statusAlert(snapshot, actionError));
  const latest = $derived(levels.at(-1) ?? 0);

  /** Takes a snapshot unless a newer one has already arrived (commands and events race). */
  function accept(next: Snapshot): void {
    if (!resetSnapshotBaseline && snapshot !== null && next.revision < snapshot.revision) return;
    resetSnapshotBaseline = false;
    // A part closed at a silence moves time into recordedMs while recording
    // goes on: shift the running start by as much, so the shown time stays continuous.
    const t = Date.now();
    if (next.phase === 'recording' && snapshot?.phase === 'recording' && recordingSince !== null && next.recordedMs > snapshot.recordedMs)
      recordingSince = t - Math.max(0, snapshot.recordedMs + t - recordingSince - next.recordedMs);
    if (next.phase === 'recording') recordingSince ??= Date.now();
    else recordingSince = null;
    if (next.phase !== 'recording' && next.phase !== 'paused') levels = [];
    if (next.phase === 'finished' && (snapshot?.phase !== 'finished' || snapshot.text !== next.text)) draft = next.text;
    const becameReady = next.phase === 'ready' && snapshot?.phase !== 'ready';
    snapshot = next;
    if (pendingRecorderFocus) {
      pendingRecorderFocus = false;
      focusRecorderAfterConnect();
    }
    if (becameReady && !webBackend) listInputs();
    if (next.libraryRevision > listedRevision) void list(next.libraryRevision);
  }

  /** Fetches the library for `revision`; only the latest fetch is kept. */
  async function list(revision: number): Promise<void> {
    listedRevision = Math.max(listedRevision, revision);
    listing += 1;
    const mine = listing;
    const gen = sessionGeneration;
    try {
      const got = await libraryList();
      if (mine !== listing || gen !== sessionGeneration) return;
      entries = got;
      libraryError = null;
    } catch (e) {
      if (mine === listing && gen === sessionGeneration) libraryError = errorText(e);
    }
  }

  function saveSettings(next: Settings, onOk: () => void, onErr: (msg: string) => void): void {
    if (webBackend && !settingsLoaded) return;
    settings = next;
    const gen = sessionGeneration;
    settingsSet(next).then(
      () => {
        if (gen === sessionGeneration) onOk();
      },
      (e: unknown) => {
        if (gen === sessionGeneration) onErr(errorText(e));
      },
    );
  }

  function toggleLibrary(): void {
    saveSettings(
      { ...settings, libraryOpen: !settings.libraryOpen },
      () => {
        toggleError = null;
      },
      (msg) => {
        toggleError = msg;
      },
    );
  }

  /** The drawer's width while it is dragged or keyed: shown at once, saved by saveLibraryWidth. */
  function resizeLibrary(width: number): void {
    settings = { ...settings, libraryWidth: width };
  }

  function saveLibraryWidth(): void {
    saveSettings(
      settings,
      () => {
        toggleError = null;
      },
      (msg) => {
        toggleError = msg;
      },
    );
  }

  function setCleanup(on: boolean): void {
    saveSettings(
      { ...settings, cleanup: on },
      () => {
        settingsError = null;
      },
      (msg) => {
        settingsError = msg;
      },
    );
  }

  function setCopySound(on: boolean): void {
    saveSettings(
      { ...settings, copySound: on },
      () => {
        settingsError = null;
      },
      (msg) => {
        settingsError = msg;
      },
    );
  }

  function setSilenceSplit(ms: number): void {
    saveSettings(
      { ...settings, silenceSplitMs: ms },
      () => {
        settingsError = null;
      },
      (msg) => {
        settingsError = msg;
      },
    );
  }

  /** Lists the installed models in the background; only the latest listing is kept. */
  function listModels(): void {
    modelListing += 1;
    const n = modelListing;
    const gen = sessionGeneration;
    models = null;
    ollamaDown = false;
    ollamaReason = '';
    ollamaModels().then(
      (got) => {
        if (n === modelListing && gen === sessionGeneration) models = got;
      },
      (e: unknown) => {
        if (n !== modelListing || gen !== sessionGeneration) return;
        ollamaDown = true;
        models = [];
        ollamaReason = errorText(e);
      },
    );
  }

  /** Lists the input devices in the background; only the latest listing is kept, and a failure keeps the last list. */
  function listInputs(): void {
    inputListing += 1;
    const n = inputListing;
    const gen = sessionGeneration;
    inputDevices().then(
      (got) => {
        if (n !== inputListing || gen !== sessionGeneration) return;
        inputs = got;
        inputsError = null;
      },
      (e: unknown) => {
        if (n === inputListing && gen === sessionGeneration) inputsError = errorText(e);
      },
    );
  }

  /** The microphone menu's choice: an input device's name, or '' for the Mac default. */
  function chooseInput(value: string): void {
    if (value === settings.inputDevice) return;
    const previous = settings.inputDevice;
    settings = { ...settings, inputDevice: value };
    settingsSet(settings).then(
      () => {
        toggleError = null;
      },
      (e: unknown) => {
        settings = { ...settings, inputDevice: previous };
        toggleError = errorText(e);
      },
    );
  }

  /** A captured global shortcut: saved at once, the engine's answer adopted, and put back when the save fails. */
  function setShortcut(value: string): void {
    const previous = settings.toggleShortcut;
    settings = { ...settings, toggleShortcut: value };
    settingsSet(settings).then(
      (saved) => {
        settings = saved;
        settingsError = null;
      },
      (e: unknown) => {
        settings = { ...settings, toggleShortcut: previous };
        settingsError = errorText(e);
      },
    );
  }

  /** A captured pause shortcut: saved at once, the engine's answer adopted, and put back when the save fails. */
  function setPauseShortcut(value: string): void {
    const previous = settings.pauseShortcut;
    settings = { ...settings, pauseShortcut: value };
    settingsSet(settings).then(
      (saved) => {
        settings = saved;
        settingsError = null;
      },
      (e: unknown) => {
        settings = { ...settings, pauseShortcut: previous };
        settingsError = errorText(e);
      },
    );
  }

  function setSummaryModel(name: string): void {
    saveSettings(
      { ...settings, summaryModel: name },
      () => {
        settingsError = null;
      },
      (msg) => {
        settingsError = msg;
      },
    );
  }

  function setCleanupModel(name: string): void {
    saveSettings(
      { ...settings, cleanupModel: name },
      () => {
        settingsError = null;
      },
      (msg) => {
        settingsError = msg;
      },
    );
  }

  function setSummaries(on: boolean): void {
    saveSettings(
      { ...settings, summaries: on },
      () => {
        settingsError = null;
      },
      (msg) => {
        settingsError = msg;
      },
    );
  }

  /** Browse…: the open panel picks another glossary file, which is saved at once. */
  function browseVocabulary(): void {
    pickVocabularyFile(settings.vocabulary).then(
      (path) => {
        if (path === null) return;
        settings = { ...settings, vocabulary: path };
        settingsSet(settings).then(
          () => {
            settingsError = null;
          },
          (e: unknown) => {
            settingsError = errorText(e);
          },
        );
      },
      (e: unknown) => {
        settingsError = errorText(e);
      },
    );
  }

  /** Open in Text Editor: the engine creates the glossary when missing, then opens it. */
  function openVocabulary(): void {
    vocabularyOpen().catch((e: unknown) => {
      settingsError = errorText(e);
    });
  }

  /** Browse…: the open panel picks another learned list, which is saved at once. */
  function browseLearnedVocabulary(): void {
    pickVocabularyFile(settings.learnedVocabulary).then(
      (path) => {
        if (path === null) return;
        settings = { ...settings, learnedVocabulary: path };
        settingsSet(settings).then(
          () => {
            settingsError = null;
          },
          (e: unknown) => {
            settingsError = errorText(e);
          },
        );
      },
      (e: unknown) => {
        settingsError = errorText(e);
      },
    );
  }

  /** Open in Text Editor: the engine creates the learned list when missing, then opens it. */
  function openLearnedVocabulary(): void {
    learnedVocabularyOpen().catch((e: unknown) => {
      settingsError = errorText(e);
    });
  }

  /** Browse…: the open panel picks another speech model folder, which is saved and re-read at once. */
  function browseModelFolder(): void {
    pickModelFolder(settings.modelFolder).then(
      (path) => {
        if (path === null) return;
        settings = { ...settings, modelFolder: path };
        settingsSet(settings).then(
          () => {
            settingsError = null;
            dictationState().then(accept, (e: unknown) => {
              settingsError = errorText(e);
            });
          },
          (e: unknown) => {
            settingsError = errorText(e);
          },
        );
      },
      (e: unknown) => {
        settingsError = errorText(e);
      },
    );
  }

  /** Whether library entry `id` is the one shown (another may have been chosen meanwhile). */
  function showing(id: string): boolean {
    return entry !== null && entry.id === id;
  }

  /** Shows library entry `chosen` in place of the recorder. */
  async function select(chosen: LibraryEntry): Promise<void> {
    const gen = sessionGeneration;
    const req = ++entryRequest;
    entry = { id: chosen.id, title: chosen.title, transcript: null };
    entryError = null;
    entryCopied = false;
    try {
      const transcript = await libraryGet(chosen.id);
      if (gen === sessionGeneration && req === entryRequest && showing(chosen.id)) {
        entry = { id: chosen.id, title: chosen.title, transcript };
      }
    } catch (e) {
      if (gen === sessionGeneration && req === entryRequest && showing(chosen.id)) {
        entryError = errorText(e);
      }
    }
  }

  function pick(chosen: LibraryEntry): void {
    void select(chosen);
  }

  async function copyEntry(id: string): Promise<boolean> {
    const gen = sessionGeneration;
    const req = entryRequest;
    entryCopied = false;
    try {
      await libraryCopy(id);
      if (gen !== sessionGeneration || req !== entryRequest || !showing(id)) return false;
      entryError = null;
      entryCopied = true;
      return true;
    } catch (e) {
      if (gen === sessionGeneration && req === entryRequest && showing(id)) {
        entryError = errorText(e);
      }
      return false;
    }
  }

  /** New: back to the recorder from an entry, and a new session when the phase allows one. */
  function newPressed(): void {
    entry = null;
    if (can('new')) void run('new');
  }

  /** A level from the microphone; one that arrives after Pause is dropped, so the waveform freezes. */
  function hear(level: number): void {
    if (snapshot?.phase === 'recording') levels = pushLevel(levels, level);
  }

  async function loadSession(): Promise<void> {
    const gen = sessionGeneration;
    loadError = null;
    try {
      const next = await dictationState();
      if (gen !== sessionGeneration) return;
      accept(next);
    } catch (e) {
      if (gen === sessionGeneration) loadError = errorText(e);
    }
  }

  function retryLoad(): void {
    if (webBackend) retryWebConnection();
    void loadSession();
  }

  function refreshAll(): void {
    const gen = sessionGeneration;
    void list(0);
    if (!webBackend) listInputs();
    // Unreadable settings leave the defaults, silently.
    settingsGet().then(
      (s) => {
        if (gen !== sessionGeneration) return;
        settings = s;
        settingsLoaded = true;
      },
      () => undefined,
    );
    void loadSession();
    // Settings shows no version line when the version cannot be read.
    appVersion().then(
      (v) => {
        if (gen === sessionGeneration) version = v;
      },
      () => undefined,
    );
    if (settingsOpen) listModels();
  }

  function resetForOriginSwitch(nextOrigin: string): void {
    activeOrigin = nextOrigin;
    sessionGeneration += 1;
    listing += 1;
    modelListing += 1;
    inputListing += 1;
    entryRequest += 1;
    snapshot = null;
    draft = '';
    recordingSince = null;
    levels = [];
    loadError = null;
    actionError = null;
    busy = false;
    entries = [];
    libraryError = null;
    listedRevision = -1;
    entry = null;
    entryError = null;
    entryCopied = false;
    settingsError = null;
    toggleError = null;
    version = '';
    models = null;
    ollamaDown = false;
    ollamaReason = '';
    inputs = null;
    inputsError = null;
    settingsLoaded = false;
    resetSnapshotBaseline = true;
  }

  function rebind(): void {
    sessionGeneration += 1;
    loadError = null;
    resetSnapshotBaseline = true;
    listedRevision = -1;
    refreshAll();
  }

  function closeSettingsForConnect(): void {
    if (settingsOpen) {
      settingsPanel?.closeDialog();
      settingsOpen = false;
    }
  }

  function openConnectFromSettings(): void {
    closeSettingsForConnect();
    connectOrigin = 'manual_settings';
    connectSubmitted = false;
    connectFeedback = null;
    connectOpen = true;
    observeWebConnectDecision({
      trigger: 'settings',
      outcome: 'ok',
      reason: 'open',
      generation: webUi.connection.generation,
    });
  }

  function openConnectFromRecovery(): void {
    closeSettingsForConnect();
    connectOrigin = 'manual_recovery';
    connectSubmitted = false;
    connectFeedback = webNeedsRecovery ? webRecoveryMessage : null;
    connectOpen = true;
    observeWebConnectDecision({
      trigger: 'recovery',
      outcome: 'ok',
      reason: 'open',
      generation: webUi.connection.generation,
    });
  }

  function openConnectAuto(
    message: string,
    trigger: 'initial_failure' | 'explicit_failure' | 'post_auth_4401',
    generation: number,
    priorAuth: boolean,
    failureCode: WebAttemptFailureCode,
  ): void {
    if (!connectOpen) {
      closeSettingsForConnect();
      connectOrigin = 'auto';
      connectSubmitted = false;
      connectOpen = true;
    }
    connectFeedback = message;
    observeWebConnectDecision({
      trigger,
      outcome: 'ok',
      reason: 'open',
      generation,
      priorAuth,
      failureCode,
    });
  }

  function submitConnect(config: WebConnectionConfig): void {
    connectFeedback = null;
    try {
      configureWebConnection(config);
      connectSubmitted = true;
    } catch {
      connectFeedback = 'Enter a valid server URL.';
    }
  }

  function retryConnect(): void {
    connectFeedback = null;
    connectSubmitted = true;
    retryWebConnection();
  }

  function focusRecorderAfterConnect(): void {
    void tick().then(() => {
      const primary = document.querySelector<HTMLButtonElement>('main .controls button:not(:disabled)');
      const newBtn = document.querySelector<HTMLButtonElement>('.band button.primary:not(:disabled)');
      (primary ?? newBtn ?? settingsButton)?.focus();
      focusFromPointer = true;
      pointerDown = false;
    });
  }

  function focusRecoveryTrigger(): void {
    void tick().then(() => {
      (recoveryConnectButton ?? settingsButton)?.focus();
      focusFromPointer = false;
      pointerDown = false;
    });
  }

  function dismissConnectForSuccess(): void {
    const origin = connectOrigin;
    connectOpen = false;
    connectOrigin = null;
    connectSubmitted = false;
    connectFeedback = null;
    if (origin === 'manual_settings') {
      settingsInitialFocus = 'connect';
      settingsOpen = true;
      listModels();
    } else {
      pendingRecorderFocus = snapshot === null;
      focusRecorderAfterConnect();
    }
  }

  function cancelConnect(): void {
    const origin = connectOrigin;
    connectOpen = false;
    connectOrigin = null;
    connectSubmitted = false;
    connectFeedback = null;
    observeWebConnectDecision({
      trigger: 'cancel',
      outcome: 'cancelled',
      reason: 'dismiss',
      generation: webUi.connection.generation,
    });
    if (origin === 'manual_settings') {
      settingsInitialFocus = 'connect';
      settingsOpen = true;
      listModels();
    } else {
      focusRecoveryTrigger();
    }
  }

  function handleWebUiState(state: WebUiState): void {
    webUi = state;
    const attempt = state.connection;

    if (activeOrigin !== null && attempt.effectiveOrigin !== activeOrigin) {
      resetForOriginSwitch(attempt.effectiveOrigin);
    } else if (activeOrigin === null) {
      activeOrigin = attempt.effectiveOrigin;
    }

    const key = `${attempt.status}:${attempt.generation}`;
    if (key === lastHandledAttemptKey) return;
    lastHandledAttemptKey = key;

    if (attempt.status === 'failed') {
      if (attempt.failureCode === 'auth_4401') {
        const trigger = attempt.priorAuth ? 'post_auth_4401' : 'initial_failure';
        openConnectAuto('Authentication refused.', trigger, attempt.generation, attempt.priorAuth, attempt.failureCode);
      } else if (attempt.failureCode !== 'disconnect') {
        if (!attempt.priorAuth) {
          openConnectAuto(
            'Could not connect to the Scriber server.',
            'initial_failure',
            attempt.generation,
            false,
            attempt.failureCode,
          );
        } else if (attempt.reason === 'configure' || attempt.reason === 'explicit_retry') {
          openConnectAuto(
            'Could not connect to the Scriber server.',
            'explicit_failure',
            attempt.generation,
            attempt.priorAuth,
            attempt.failureCode,
          );
        } else {
          observeWebConnectDecision({
            trigger: 'transient_reconnect',
            outcome: 'skip',
            reason: 'suppress',
            generation: attempt.generation,
            priorAuth: attempt.priorAuth,
            failureCode: attempt.failureCode,
          });
        }
      }
    } else if (attempt.status === 'authenticated' && connectOpen) {
      if (connectOrigin === 'auto' || connectSubmitted) {
        observeWebConnectDecision({
          trigger: 'active_auth',
          outcome: 'ok',
          reason: 'dismiss',
          generation: attempt.generation,
          priorAuth: attempt.priorAuth,
        });
        dismissConnectForSuccess();
      } else {
        observeWebConnectDecision({
          trigger: 'background_auth',
          outcome: 'skip',
          reason: 'keep_open',
          generation: attempt.generation,
          priorAuth: attempt.priorAuth,
        });
      }
    }
  }

  async function run(action: Action): Promise<boolean> {
    busy = true;
    const gen = sessionGeneration;
    try {
      const next = await act(action, action === 'copy' ? draft : '');
      if (gen === sessionGeneration) {
        accept(next);
        actionError = null;
      }
      return true;
    } catch (e) {
      if (gen === sessionGeneration) {
        if (e instanceof WebCopyError && e.stage === 'clipboard') {
          actionError = null;
          return false;
        }
        actionError = errorText(e);
        if (e instanceof WebCopyError && e.stage === 'learning') {
          return true;
        }
      }
      return false;
    } finally {
      busy = false;
    }
  }

  // The root component lives as long as the window, so the subscriptions are
  // never undone.
  onMount(() => {
    void onDictationChanged(accept);
    void onLevel(hear);
    void onFullscreenChanged((on) => {
      fullscreen = on;
    });
    if (webBackend) {
      onWebUiStateChanged(handleWebUiState);
      onWebRebind(() => {
        rebind();
      });
    }
    // An unreadable state leaves the windowed inset, silently.
    windowFullscreen().then(
      (on) => {
        fullscreen = on;
      },
      () => undefined,
    );
    refreshAll();
  });

  $effect(() => {
    if (recordingSince === null) return;
    const timer = setInterval(() => {
      now = Date.now();
    }, 250);
    return () => {
      clearInterval(timer);
    };
  });

  // A focused button left disabled once an action settles (New, once
  // ready) would keep the focus and swallow every key after it,
  // shortcuts included; hand the focus back to the window instead.
  $effect(() => {
    if (!busy && snapshot !== null) {
      const focused = document.activeElement;
      if (focused instanceof HTMLButtonElement && focused.disabled) focused.blur();
    }
  });

  function can(action: Action): boolean {
    if (snapshot === null || busy) return false;
    if (action === 'downloadModel') return snapshot.modelError !== null && snapshot.download === null;
    const modelReady = snapshot.modelError === null && snapshot.download === null;
    if ((action === 'record' || action === 'resume') && !modelReady) return false;
    return allowed(snapshot.phase, action);
  }

  /** Where Space and Return keep their own meaning: text fields, and a button reached with the keyboard (Tab). */
  function ownsKeys(target: EventTarget | null): boolean {
    if (!(target instanceof HTMLElement)) return false;
    if (target.isContentEditable || target.matches(TEXT_FIELDS)) return true;
    return target instanceof HTMLButtonElement && !focusFromPointer;
  }

  function openSettings(fromPointer: boolean): void {
    settingsInitialFocus = 'default';
    settingsOpen = true;
    listModels();
    settingsFromPointer = fromPointer;
  }

  function settingsClosed(): void { settingsOpen = false; settingsButton?.focus(); focusFromPointer = settingsFromPointer; pointerDown = false; }

  // In-window shortcuts: ⌘, opens Settings (in every phase, unless Settings is open or the focus is in a menu); ⌘N is New, Space is Record, Pause and Resume, Return
  // is Done. A key whose action the phase does not allow does nothing here.
  // Handling one cancels its default, so a button that merely has focus from
  // a click is not clicked as well; a button reached with Tab keeps Space and
  // Return for itself. A held key's repeats are swallowed.
  function onKeydown(e: KeyboardEvent): void {
    if (e.key === 'Tab') pointerDown = false;
    if (settingsOpen || connectOpen) return;
    if (e.target instanceof Element && e.target.closest('[role="menu"]') !== null) return;
    if (opensSettings(e)) {
      e.preventDefault();
      if (!e.repeat) openSettings(true);
      return;
    }
    if (entry !== null) {
      // An entry is shown: only ⌘N (back to the recorder) acts.
      if (shortcut(e, 'idle', true) === 'new') {
        e.preventDefault();
        if (!e.repeat) newPressed();
      }
      return;
    }
    if (snapshot === null) return;
    const action = shortcut(e, snapshot.phase, ownsKeys(e.target));
    if (action === null) return;
    e.preventDefault();
    swallowSpaceUp = e.key === ' ';
    if (!e.repeat && can(action)) void run(action);
  }

  function onKeyup(e: KeyboardEvent): void {
    if (e.key !== ' ' || !swallowSpaceUp) return;
    swallowSpaceUp = false;
    e.preventDefault();
  }
</script>

<svelte:window
  onkeydown={onKeydown}
  onkeyup={onKeyup}
  onpointerdown={() => {
    pointerDown = true;
  }}
  onfocusin={() => {
    focusFromPointer = pointerDown;
    pointerDown = false;
  }}
/>

<div class="window">
  <!-- The recorder (or an entry) comes first in the document, so Tab reaches
       its controls before the band's; the grid shows the band on top. -->
  <main>
    {#if entry !== null}
      <h2 class="entry-heading">{entry.title}</h2>
      {#if entry.transcript !== null}
        <div class="transcript">
          <label for="entry-transcript">Transcript</label>
          <div class="transcript-box">
            <textarea id="entry-transcript" readonly value={entry.transcript}></textarea>
            {#key entry.id}<CopyButton copy={() => copyEntry(entry?.id ?? '')} />{/key}
          </div>
        </div>
      {/if}
      <div class="status-row">
        <p class="entry-status" class:visually-hidden={entryError !== null} aria-live="polite">{entryCopied ? 'Copied to the clipboard.' : ''}</p>
        {#if entryError !== null}
          <p class="error" role="alert" title={entryError}>{entryError}</p>
        {/if}
      </div>
    {:else if webBackend && snapshot === null && (loadError !== null || webNeedsRecovery)}
      <p class="error" role="alert">{webRecoveryMessage}</p>
      <div class="controls">
        <button
          type="button"
          class="primary"
          bind:this={recoveryConnectButton}
          onclick={openConnectFromRecovery}
        >
          Connect
        </button>
        <button type="button" onclick={retryLoad}>Try again</button>
      </div>
    {:else if loadError !== null}
      <p class="error" role="alert">Could not read the session: {loadError}</p>
      <button type="button" onclick={retryLoad}>Try again</button>
    {:else if snapshot !== null}
      <div class="status-row" class:wrap={snapshot.phase === 'finished' && snapshot.text === '' && alert === null}>
        <p class="status" aria-live="polite" title={line} class:visually-hidden={alert !== null}>{line}</p>
        {#if alert !== null}
          <p class="error" role="alert" title={alert}>{alert}</p>
        {/if}
        {#if snapshot.modelError !== null}
          <button type="button" class="row-action" onclick={() => run('downloadModel')} disabled={!can('downloadModel')}>Retry download</button>
        {/if}
        {#if snapshot.phase === 'finished' && snapshot.learned.length > 0}
          <button type="button" class="row-action" disabled={busy} onclick={() => run('undoLearn')}>Undo</button>
        {/if}
      </div>

      {#if snapshot.phase === 'ready' || snapshot.phase === 'recording' || snapshot.phase === 'paused'}
        {#if webBackend}
          {@const micName = snapshot.microphone ?? 'Browser microphone'}
          <div class="mic-row">
            <span class="mic-indicator" title={micName}>
              <svg aria-hidden="true" viewBox="0 0 16 16" width="16" height="16">
                <rect x="5.5" y="1.5" width="5" height="8.5" rx="2.5" fill="none" stroke="currentColor" />
                <path d="M3 7.5a5 5 0 0 0 10 0" fill="none" stroke="currentColor" />
                <line x1="8" y1="12.5" x2="8" y2="14.5" stroke="currentColor" />
              </svg>
              <span class="mic-name">{micName}</span>
            </span>
          </div>
        {:else}
          <MicrophoneMenu
            name={activeMicrophone(snapshot, inputs, settings.inputDevice)}
            {inputs}
            saved={settings.inputDevice}
            error={inputsError}
            onopen={listInputs}
            onchoose={chooseInput}
          />
        {/if}
      {/if}

      <div class="controls">
        {#each controlsFor(snapshot.phase) as control (control.action)}
          {@const keys = SHORTCUTS[control.action]}
          <button
            type="button"
            class={control.rank}
            disabled={!can(control.action)}
            aria-keyshortcuts={keys?.aria}
            onclick={(e) => {
              if (e.detail <= 1) void run(control.action);
            }}
          >
            {control.label}
            {#if keys !== undefined}<kbd aria-hidden="true">{keys.hint}</kbd>{/if}
          </button>
        {/each}
      </div>

      {#if metering}
        <div class="meter" class:frozen={snapshot.phase === 'paused'}>
          <div class="wave" aria-hidden="true" data-count={levels.length} data-slots={barCount(waveWidth)} bind:clientWidth={waveWidth}>
            {#each bars(levels, barCount(waveWidth)) as level, i (i)}
              <span class="bar" style:--level={level}></span>
            {/each}
          </div>
          <div class="level" aria-hidden="true">
            <span class="level-fill" style:--level={latest}></span>
          </div>
          <span class="visually-hidden">
            {snapshot.phase === 'paused'
              ? 'The microphone is off while paused.'
              : latest > 0
                ? 'The microphone is picking up sound.'
                : 'Listening for sound from the microphone.'}
          </span>
        </div>
        {#if snapshot.partial !== ''}
          <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
          <div class="partial" role="region" aria-label="Heard so far" tabindex="0">{snapshot.partial}</div>
        {/if}
      {/if}

      {#if snapshot.phase === 'finished'}
        <div class="transcript">
          <label for="transcript">Transcript</label>
          <div class="transcript-box">
            <textarea id="transcript" bind:value={draft} spellcheck="true"></textarea>
            <CopyButton disabled={!can('copy') || draft === ''} copy={() => run('copy')} />
          </div>
        </div>
      {/if}

      <!-- The download block stays the last child of the dictation branch, so its appearing moves nothing; any new in-pane region goes ABOVE it. In-state messages go in .status-row; expectNoShift in tests/e2e/helpers.ts guards both. -->
      {#if snapshot.download !== null}
        <div class="download">
          <p class="download-text" id="download-text">{downloadText(snapshot.download)}</p>
          <progress max="100" value={percent(snapshot.download)} aria-labelledby="download-text"></progress>
        </div>
      {/if}
    {/if}
  </main>

  <LibraryDrawer
    {entries}
    open={settings.libraryOpen}
    selected={entry?.id ?? null}
    error={libraryError}
    width={settings.libraryWidth}
    onselect={pick}
    onresize={resizeLibrary}
    onresizeend={saveLibraryWidth}
  />

  <header class="band" class:fullscreen data-tauri-drag-region>
    <button
      type="button"
      class="icon"
      aria-label="Library"
      aria-expanded={settings.libraryOpen}
      aria-controls="library"
      onclick={toggleLibrary}
    >
      <svg aria-hidden="true" viewBox="0 0 16 16" width="16" height="16">
        <rect x="1.5" y="2.5" width="13" height="11" rx="2" fill="none" stroke="currentColor" />
        <line x1="6" y1="2.5" x2="6" y2="13.5" stroke="currentColor" />
      </svg>
    </button>
    <h1 class="visually-hidden">Dictation</h1>
    <div class="band-spacer" data-tauri-drag-region></div>
    {#if toggleError !== null}
      <p class="error" role="alert" title={toggleError}>{toggleError}</p>
    {/if}
    {#if snapshot !== null && snapshot.phase !== 'idle' && entry === null}
      <span class="time" class:live={snapshot.phase === 'recording'} title="Recorded time" data-tauri-drag-region>
        <span class="visually-hidden">Recorded&nbsp;</span>{formatElapsed(elapsed)}
      </span>
    {/if}
    {#if webBackend && snapshot !== null && webNeedsRecovery}
      <button
        type="button"
        class="primary"
        bind:this={recoveryConnectButton}
        onclick={openConnectFromRecovery}
      >
        Connect
      </button>
    {/if}
    <button
      type="button"
      class:primary={snapshot?.phase === 'idle' && entry === null && !webNeedsRecovery}
      onclick={newPressed}
      disabled={entry === null && !can('new')} aria-keyshortcuts={SHORTCUTS.new?.aria}>
      New<kbd aria-hidden="true">{SHORTCUTS.new?.hint}</kbd>
    </button>
    <button
      type="button"
      class="icon"
      aria-label="Settings"
      aria-haspopup="dialog"
      bind:this={settingsButton}
      aria-keyshortcuts={SETTINGS_SHORTCUT.aria}
      title={`Settings (${SETTINGS_SHORTCUT.hint})`}
      onclick={(e) => { openSettings(e.detail > 0); }}
    >
      <svg aria-hidden="true" viewBox="0 0 16 16" width="16" height="16">
        <path fill="currentColor" fill-rule="evenodd" d="M6.11 3.00 6.81 0.85 9.19 0.85 9.89 3.00 10.20 3.12 12.22 2.10 13.90 3.78 12.88 5.80 13.00 6.11 15.15 6.81 15.15 9.19 13.00 9.89 12.88 10.20 13.90 12.22 12.22 13.90 10.20 12.88 9.89 13.00 9.19 15.15 6.81 15.15 6.11 13.00 5.80 12.88 3.78 13.90 2.10 12.22 3.12 10.20 3.00 9.89 0.85 9.19 0.85 6.81 3.00 6.11 3.12 5.80 2.10 3.78 3.78 2.10 5.80 3.12ZM8 5.65a2.35 2.35 0 1 0 0 4.7a2.35 2.35 0 1 0 0 -4.7Z" />
      </svg>
    </button>
  </header>

  <SettingsPanel
    bind:this={settingsPanel}
    {settings}
    open={settingsOpen}
    error={settingsError}
    {models}
    {ollamaDown}
    {ollamaReason}
    {version}
    web={webBackend}
    settingsReady={settingsLoaded}
    effectiveOrigin={webUi.connection.effectiveOrigin}
    initialFocus={settingsInitialFocus}
    onconnect={openConnectFromSettings}
    onsummaries={setSummaries}
    onsummarymodel={setSummaryModel}
    oncleanupmodel={setCleanupModel}
    oncleanup={setCleanup}
    onbrowse={browseVocabulary}
    onopenvocabulary={openVocabulary}
    onbrowselearned={browseLearnedVocabulary}
    onopenlearned={openLearnedVocabulary}
    onbrowsemodel={browseModelFolder}
    onsilencesplit={setSilenceSplit}
    onshortcut={setShortcut}
    onpauseshortcut={setPauseShortcut}
    oncopysound={setCopySound}
    onclose={settingsClosed}
  />

  {#if webBackend}
    <ConnectPanel
      open={connectOpen}
      effectiveOrigin={webUi.connection.effectiveOrigin}
      feedback={connectFeedback}
      connecting={connectConnecting}
      canRetry={connectCanRetry}
      onsubmit={submitConnect}
      onretry={retryConnect}
      oncancel={cancelConnect}
    />
  {/if}
</div>

<style>
  .mic-indicator {
    display: inline-flex;
    align-items: center;
    max-width: 100%;
    min-width: 0;
    gap: 6px;
    padding: 4px 0;
    color: var(--muted);
  }
</style>
