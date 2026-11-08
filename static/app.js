(() => {
  "use strict";

  /* ---------------------------------------------------------------------- */
  /* Helpers                                                                */
  /* ---------------------------------------------------------------------- */

  const $ = (selector, root = document) => root.querySelector(selector);
  const $$ = (selector, root = document) => [...root.querySelectorAll(selector)];

  const TOKEN_KEY = "datatask_api_token";
  const PREFS_KEY = "datatask.prefs";
  const DEFAULTS = { autoRefresh: 30, logPoll: 2, animations: true, compact: false, wrapLogs: true };
  const MAX_LOG_TEXT = 2_000_000;

  const VIEW_META = {
    tarefas: { title: "Tarefas", subtitle: "Crie, acompanhe e execute suas automações." },
    logs: { title: "Logs de execução", subtitle: "Acompanhe a saída dos comandos em tempo real." },
    historico: { title: "Histórico", subtitle: "Tentativas registradas, com filtros e paginação." },
    config: { title: "Configurações", subtitle: "Conexão, preferências e informações do servidor." }
  };

  const RESULT_META = {
    success: { label: "Sucesso", icon: "#i-check", className: "result-success" },
    error: { label: "Falhou", icon: "#i-alert", className: "result-error" },
    timeout: { label: "Timeout", icon: "#i-clock", className: "result-timeout" }
  };

  const MONTHS = ["jan", "fev", "mar", "abr", "mai", "jun", "jul", "ago", "set", "out", "nov", "dez"];
  const WEEKDAY_SHORT = ["Dom", "Seg", "Ter", "Qua", "Qui", "Sex", "Sáb"];
  const WEEKDAY_LABELS = [["0", "Domingo"], ["1", "Segunda"], ["2", "Terça"], ["3", "Quarta"], ["4", "Quinta"], ["5", "Sexta"], ["6", "Sábado"]];
  const WEEKDAY_IDS = ["wd0", "wd1", "wd2", "wd3", "wd4", "wd5", "wd6"];

  const el = {
    connCard: $("#connCard"),
    connTitle: $("#connTitle"),
    connSub: $("#connSub"),
    sidebarVersion: $("#sidebarVersion"),
    connectBanner: $("#connectBanner"),
    quickConnectForm: $("#quickConnectForm"),
    quickToken: $("#quickToken"),
    sidebar: $("#sidebar"),
    scrim: $("#sidebarScrim"),
    menuToggle: $("#menuToggle"),
    viewTitle: $("#viewTitle"),
    viewSubtitle: $("#viewSubtitle"),
    newJobButton: $("#newJobButton"),
    globalRefresh: $("#globalRefresh"),
    navActiveCount: $("#navActiveCount"),
    searchInput: $("#searchInput"),
    statusFilter: $("#statusFilter"),
    tasksSummary: $("#tasksSummary"),
    jobsBody: $("#jobsBody"),
    jobsPage: $("#jobsPage"),
    jobsPrev: $("#jobsPrev"),
    jobsNext: $("#jobsNext"),
    exportJobs: $("#exportJobs"),
    importJobs: $("#importJobs"),
    importFile: $("#importFile"),
    statTotal: $("#statTotal"),
    statActive: $("#statActive"),
    statRecurring: $("#statRecurring"),
    statToday: $("#statToday"),
    logJobSearch: $("#logJobSearch"),
    logJobList: $("#logJobList"),
    logTitle: $("#logTitle"),
    logMeta: $("#logMeta"),
    logResult: $("#logResult"),
    livePill: $("#livePill"),
    logFilter: $("#logFilter"),
    logLive: $("#logLive"),
    logAutoScroll: $("#logAutoScroll"),
    logWrap: $("#logWrap"),
    logRefresh: $("#logRefresh"),
    logDownload: $("#logDownload"),
    logClear: $("#logClear"),
    logOutput: $("#logOutput"),
    logSize: $("#logSize"),
    logUpdated: $("#logUpdated"),
    historyJobFilter: $("#historyJobFilter"),
    historyResultFilter: $("#historyResultFilter"),
    historyRefresh: $("#historyRefresh"),
    historyBody: $("#historyBody"),
    historySummary: $("#historySummary"),
    historyPage: $("#historyPage"),
    historyPrev: $("#historyPrev"),
    historyNext: $("#historyNext"),
    tokenInput: $("#tokenInput"),
    connectionForm: $("#connectionForm"),
    disconnectButton: $("#disconnectButton"),
    prefAutoRefresh: $("#prefAutoRefresh"),
    prefLogPoll: $("#prefLogPoll"),
    prefAnimations: $("#prefAnimations"),
    prefCompact: $("#prefCompact"),
    prefWrap: $("#prefWrap"),
    infoRefresh: $("#infoRefresh"),
    serverInfo: $("#serverInfo"),
    backupsRefresh: $("#backupsRefresh"),
    backupsVerify: $("#backupsVerify"),
    backupsExportJson: $("#backupsExportJson"),
    backupsExportCsv: $("#backupsExportCsv"),
    backupsSendReport: $("#backupsSendReport"),
    backupHealthStatus: $("#backupHealthStatus"),
    backupHealthTitle: $("#backupHealthTitle"),
    backupHealthDetails: $("#backupHealthDetails"),
    backupsSummary: $("#backupsSummary"),
    backupsBody: $("#backupsBody"),
    backupVerificationsSummary: $("#backupVerificationsSummary"),
    backupVerificationsBody: $("#backupVerificationsBody"),
    jobDialog: $("#jobDialog"),
    jobForm: $("#jobForm"),
    formTitle: $("#formTitle"),
    jobName: $("#jobName"),
    jobDescription: $("#jobDescription"),
    jobStatus: $("#jobStatus"),
    scheduleType: $("#scheduleType"),
    scheduleFields: $("#scheduleFields"),
    schedulePreview: $("#schedulePreview"),
    scheduleHelp: $("#scheduleHelp"),
    jobAction: $("#jobAction"),
    jobWorkingDir: $("#jobWorkingDir"),
    jobTimeout: $("#jobTimeout"),
    jobRetries: $("#jobRetries"),
    jobRetryDelay: $("#jobRetryDelay"),
    jobNotifyUrl: $("#jobNotifyUrl"),
    saveJobButton: $("#saveJobButton"),
    closeJobDialog: $("#closeJobDialog"),
    cancelJobButton: $("#cancelJobButton"),
    confirmDialog: $("#confirmDialog"),
    confirmTitle: $("#confirmTitle"),
    confirmMessage: $("#confirmMessage"),
    confirmAccept: $("#confirmAccept"),
    confirmCancel: $("#confirmCancel"),
    toasts: $("#toasts")
  };

  const state = {
    jobs: [],
    pickerJobs: [],
    running: new Set(),
    info: null,
    view: "tarefas",
    sort: { key: "name", dir: 1 },
    jobsPage: { limit: 25, offset: 0, total: 0, q: "", status: "todas" },
    editing: null,
    editingVersion: null,
    loadedOnce: false,
    logs: {
      job: null,
      text: "",
      offset: 0,
      size: 0,
      updatedAt: 0,
      live: true,
      autoScroll: true,
      filter: ""
    },
    history: {
      items: [],
      total: 0,
      offset: 0,
      limit: 25,
      job: "",
      result: "",
      enabled: true
    },
    timers: { auto: null, log: null, watch: null, search: null }
  };

  let prefs = loadPrefs();
  let logPolling = false;

  /* ---------------------------------------------------------------------- */
  /* Storage                                                                */
  /* ---------------------------------------------------------------------- */

  function getToken() {
    try { return sessionStorage.getItem(TOKEN_KEY) || ""; } catch (_) { return ""; }
  }

  function setToken(value) {
    try {
      if (value) sessionStorage.setItem(TOKEN_KEY, value);
      else sessionStorage.removeItem(TOKEN_KEY);
    } catch (_) { /* storage indisponível */ }
  }

  function loadPrefs() {
    try {
      return { ...DEFAULTS, ...JSON.parse(localStorage.getItem(PREFS_KEY) || "{}") };
    } catch (_) {
      return { ...DEFAULTS };
    }
  }

  function savePrefs() {
    try { localStorage.setItem(PREFS_KEY, JSON.stringify(prefs)); } catch (_) { /* ignore */ }
  }

  /* ---------------------------------------------------------------------- */
  /* Formatação                                                             */
  /* ---------------------------------------------------------------------- */

  function escapeHtml(value) {
    return String(value ?? "").replace(/[&<>"']/g, character => ({
      "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;"
    })[character]);
  }

  const pad2 = value => String(value).padStart(2, "0");

  function formatBytes(bytes) {
    if (!Number.isFinite(bytes) || bytes <= 0) return "0 B";
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(2)} MB`;
  }

  function timeAgo(value) {
    let then;
    if (typeof value === "number") then = new Date(value);
    else if (value instanceof Date) then = value;
    else {
      const parsed = String(value).match(/^(\d{4})-(\d{2})-(\d{2}) (\d{2}):(\d{2})$/);
      if (!parsed) return String(value ?? "—");
      then = new Date(+parsed[1], +parsed[2] - 1, +parsed[3], +parsed[4], +parsed[5]);
    }
    const minutes = Math.floor((Date.now() - then.getTime()) / 60000);
    if (minutes < 0) return "agora mesmo";
    if (minutes < 1) return "agora mesmo";
    if (minutes < 60) return `há ${minutes} min`;
    const hours = Math.floor(minutes / 60);
    if (hours < 24) return `há ${hours}h ${minutes % 60}min`;
    const days = Math.floor(hours / 24);
    if (days < 30) return `há ${days} dia${days > 1 ? "s" : ""}`;
    return formatDateTime(then);
  }

  function formatDateTime(date) {
    if (!(date instanceof Date) || Number.isNaN(date.getTime())) return null;
    return `${date.getDate()} ${MONTHS[date.getMonth()]} ${pad2(date.getHours())}:${pad2(date.getMinutes())}`;
  }

  function parseOnceDate(value) {
    let match = String(value).match(/^(\d{4})-(\d{2})-(\d{2}) (\d{2}):(\d{2})$/);
    if (match) return new Date(+match[1], +match[2] - 1, +match[3], +match[4], +match[5]);
    match = String(value).match(/^(\d{1,2}) (\d{1,2}) (\d{4}) (\d{1,2}) (\d{1,2})$/);
    if (match) return new Date(+match[3], +match[2] - 1, +match[1], +match[4], +match[5]);
    return null;
  }

  function isCron(value) {
    return String(value).trim().split(/\s+/).length === 5;
  }

  /* ---------------------------------------------------------------------- */
  /* Descrição legível de agendamentos e próxima execução                   */
  /* ---------------------------------------------------------------------- */

  function describeSchedule(value) {
    const raw = String(value ?? "").trim();
    const once = parseOnceDate(raw);
    if (once) return `Uma vez em ${formatDateTime(once)}`;

    const fields = raw.split(/\s+/);
    if (fields.length !== 5) return raw;
    const [minute, hour, day, month, weekday] = fields;
    if (month !== "*") return raw;

    const hhmm = (h, m) => `${pad2(h)}:${pad2(m)}`;
    const weekdayNames = part => {
      if (part === "1-5") return "dias úteis";
      if (part === "0,6" || part === "6,0") return "fins de semana";
      if (part === "0-6" || part === "*") return "todos os dias";
      const days = [];
      for (const piece of part.split(",")) {
        if (piece.includes("-")) {
          const [start, end] = piece.split("-").map(Number);
          if (start > end || start < 0 || end > 7) return null;
          for (let index = start; index <= end; index++) days.push(index === 7 ? 0 : index);
        } else {
          const index = Number(piece);
          if (!Number.isInteger(index) || index < 0 || index > 7) return null;
          days.push(index === 7 ? 0 : index);
        }
      }
      const unique = [...new Set(days)];
      if (!unique.length) return null;
      const names = unique.map(index => WEEKDAY_SHORT[index]);
      if (names.length === 1) return `${names[0]}`;
      return `${names.slice(0, -1).join(", ")} e ${names[names.length - 1]}`;
    };

    if (day === "*" && weekday === "*") {
      if (hour === "*" && minute === "*") return "A cada minuto";
      if (hour === "*" && /^\*\/(\d+)$/.test(minute)) return `A cada ${minute.slice(2)} minutos`;
      if (hour === "*" && /^\d+$/.test(minute)) return `A cada hora, aos ${minute} minutos`;
      const everyMinute = minute.match(/^\*\/(\d+)$/);
      const everyHour = hour.match(/^\*\/(\d+)$/);
      if (everyMinute && /^\d+-\d+$/.test(hour)) {
        return `A cada ${everyMinute[1]} min entre ${hour.split("-")[0]}h e ${hour.split("-")[1]}h`;
      }
      const hourRange = hour.match(/^(\d+)-(\d+)\/(\d+)$/);
      if (minute === "0" && hourRange) {
        return `A cada ${hourRange[3]} h entre ${hourRange[1]}h e ${hourRange[2]}h`;
      }
      if (minute === "0" && everyHour) return `A cada ${everyHour[1]} horas`;
      if (/^\d+$/.test(minute) && /^[\d,]+$/.test(hour) && hour.includes(",")) {
        const times = hour.split(",")
          .filter(part => /^\d+$/.test(part))
          .map(part => hhmm(Number(part), Number(minute)));
        if (times.length) return `Diariamente às ${times.join(", ")}`;
      }
      if (/^\d+$/.test(minute) && /^\d+$/.test(hour)) {
        return `Todos os dias às ${hhmm(Number(hour), Number(minute))}`;
      }
      return raw;
    }

    if (day === "*" && weekday !== "*" && /^\d+$/.test(minute) && /^\d+$/.test(hour)) {
      const names = weekdayNames(weekday);
      if (names) return `${names} às ${hhmm(Number(hour), Number(minute))}`;
    }

    if (day !== "*" && weekday === "*" && /^\d+$/.test(minute) && /^\d+$/.test(hour) && /^\d+$/.test(day)) {
      return `Todo dia ${day} do mês às ${hhmm(Number(hour), Number(minute))}`;
    }

    return raw;
  }

  function cronFieldValues(field, minimum, maximum) {
    const values = new Set();
    if (!field) return null;
    for (const part of field.split(",")) {
      if (!part) return null;
      const [base, stepText] = part.split("/");
      const step = stepText === undefined ? 1 : Number(stepText);
      if (!Number.isInteger(step) || step < 1) return null;
      let start;
      let end;
      if (base === "*") {
        start = minimum;
        end = maximum;
      } else if (base.includes("-")) {
        const [rangeStart, rangeEnd] = base.split("-").map(Number);
        if (!Number.isInteger(rangeStart) || !Number.isInteger(rangeEnd)
          || rangeStart < minimum || rangeEnd > maximum || rangeStart > rangeEnd) return null;
        start = rangeStart;
        end = rangeEnd;
      } else {
        const value = Number(base);
        if (!Number.isInteger(value) || value < minimum || value > maximum) return null;
        start = value;
        end = stepText === undefined ? value : maximum;
      }
      for (let value = start; value <= end; value += step) {
        values.add(maximum === 7 && value === 7 ? 0 : value);
      }
    }
    return values;
  }

  function nextRunEstimate(schedule) {
    const once = parseOnceDate(schedule);
    if (once) return once;
    if (!isCron(schedule)) return null;
    const fields = schedule.trim().split(/\s+/);
    const minutes = cronFieldValues(fields[0], 0, 59);
    const hours = cronFieldValues(fields[1], 0, 23);
    const days = cronFieldValues(fields[2], 1, 31);
    const months = cronFieldValues(fields[3], 1, 12);
    const weekdays = cronFieldValues(fields[4], 0, 7);
    if (!minutes || !hours || !days || !months || !weekdays) return null;
    const dayWildcard = fields[2] === "*";
    const weekdayWildcard = fields[4] === "*";
    const cursor = new Date();
    cursor.setSeconds(0, 0);
    for (let steps = 0; steps < 366 * 24 * 60; steps++) {
      cursor.setMinutes(cursor.getMinutes() + 1);
      if (!minutes.has(cursor.getMinutes()) || !hours.has(cursor.getHours())
        || !months.has(cursor.getMonth() + 1)) continue;
      const weekDay = cursor.getDay();
      const dayMatches = dayWildcard && weekdayWildcard
        ? true
        : !dayWildcard && !weekdayWildcard
          ? days.has(cursor.getDate()) || weekdays.has(weekDay)
          : dayWildcard
            ? weekdays.has(weekDay)
            : days.has(cursor.getDate());
      if (dayMatches) return cursor;
    }
    return null;
  }

  /* ---------------------------------------------------------------------- */
  /* API                                                                    */
  /* ---------------------------------------------------------------------- */

  async function api(path, options = {}) {
    const token = getToken();
    if (!token) throw new Error("Informe o token da API para continuar.");
    const headers = new Headers(options.headers || {});
    headers.set("Authorization", `Bearer ${token}`);
    if (options.body && !headers.has("Content-Type")) headers.set("Content-Type", "application/json");
    const response = await fetch(path, { ...options, headers });
    if (!response.ok) {
      let message = `Erro HTTP ${response.status}`;
      try {
        const body = await response.json();
        if (body.error) message = body.error;
        if (Array.isArray(body.errors) && body.errors.length) {
          const first = body.errors[0];
          const subject = first.name || `item ${first.index}`;
          message += ` (${subject}: ${first.error})`;
        }
      } catch (_) { /* resposta sem JSON */ }
      if (response.status === 401) {
        setToken("");
        setConnection("offline", "Token inválido");
      }
      const error = new Error(message);
      error.status = response.status;
      throw error;
    }
    if (response.status === 204) return null;
    const contentType = response.headers.get("content-type") || "";
    if (contentType.includes("application/json")) return response.json();
    return response.text();
  }

  const encodedName = name => encodeURIComponent(name);

  /* ---------------------------------------------------------------------- */
  /* Conexão e preferências                                                 */
  /* ---------------------------------------------------------------------- */

  function setConnection(status, message) {
    el.connCard.classList.toggle("online", status === "online");
    el.connCard.classList.toggle("offline", status !== "online");
    el.connTitle.textContent = status === "online"
      ? "API conectada"
      : status === "error" ? "Falha na conexão" : "Desconectado";
    el.connSub.textContent = message;
    el.connectBanner.hidden = status === "online" || !!getToken();
    if (status === "online") el.quickToken.value = "";
  }

  function applyPrefs() {
    document.documentElement.classList.toggle("no-motion", !prefs.animations);
    document.body.classList.toggle("compact", !!prefs.compact);
    el.logOutput.classList.toggle("wrap", !!prefs.wrapLogs);
    el.prefAutoRefresh.value = String(prefs.autoRefresh);
    el.prefLogPoll.value = String(prefs.logPoll);
    el.prefAnimations.checked = !!prefs.animations;
    el.prefCompact.checked = !!prefs.compact;
    el.prefWrap.checked = !!prefs.wrapLogs;
    el.logWrap.checked = !!prefs.wrapLogs;
    scheduleAutoRefresh();
    if (state.view === "logs" && state.logs.job) startLogPolling();
  }

  function updatePrefs(patch) {
    prefs = { ...prefs, ...patch };
    savePrefs();
    applyPrefs();
  }

  function scheduleAutoRefresh() {
    clearInterval(state.timers.auto);
    state.timers.auto = null;
    if (!prefs.autoRefresh || prefs.autoRefresh < 5) return;
    state.timers.auto = setInterval(() => {
      if (!getToken()) return;
      if (document.hidden || el.jobDialog.open || el.confirmDialog.open) return;
      refreshData({ silent: true }).catch(() => setConnection("error", "Erro de conexão"));
    }, prefs.autoRefresh * 1000);
  }

  /* ---------------------------------------------------------------------- */
  /* Dados                                                                  */
  /* ---------------------------------------------------------------------- */

  function showTableSkeleton() {
    el.jobsBody.innerHTML = Array.from({ length: 4 }, () => `
      <tr class="skeleton-row">
        <td><div class="skeleton w-60"></div><div class="skeleton w-40" style="margin-top:7px"></div></td>
        <td><div class="skeleton w-80"></div></td>
        <td><div class="skeleton w-40"></div></td>
        <td><div class="skeleton w-60"></div></td>
        <td><div class="skeleton w-40" style="margin-left:auto"></div></td>
      </tr>`).join("");
  }

  async function refreshData({ silent = true } = {}) {
    if (!getToken()) {
      setConnection("offline", "Informe o token da API");
      return;
    }
    if (!state.loadedOnce && !silent) showTableSkeleton();
    const params = new URLSearchParams({
      limit: String(state.jobsPage.limit),
      offset: String(state.jobsPage.offset),
      sort: state.sort.key,
      order: state.sort.dir === 1 ? "asc" : "desc"
    });
    if (state.jobsPage.q) params.set("q", state.jobsPage.q);
    if (state.jobsPage.status && state.jobsPage.status !== "todas") {
      params.set("status", state.jobsPage.status);
    }
    const [page, info] = await Promise.all([
      api(`/api/jobs?${params.toString()}`),
      api("/api/info")
    ]);
    state.jobs = Array.isArray(page?.items) ? page.items : [];
    state.jobsPage.total = Number(page?.total) || 0;
    state.jobsPage.limit = Number(page?.limit) || state.jobsPage.limit;
    state.jobsPage.offset = Number(page?.offset) || 0;
    if (state.view === "logs" || state.view === "historico") {
      try {
        const allJobs = await api("/api/jobs");
        state.pickerJobs = Array.isArray(allJobs) ? allJobs : [];
      } catch (_) { /* mantém o picker anterior */ }
    }
    state.info = info;
    state.running = new Set(info?.running || []);
    state.loadedOnce = true;
    setConnection("online", "Sessão autenticada");
    el.sidebarVersion.textContent = `DataTask · v${info?.version || "—"}`;
    renderAll();
    const picker = state.pickerJobs.length ? state.pickerJobs : state.jobs;
    if (state.logs.job && !picker.some(job => job.name === state.logs.job)) {
      clearLogSelection();
    } else if (state.logs.job) {
      updateLogMeta();
    }
  }

  function renderAll() {
    renderStats();
    renderTasks();
    renderLogJobList();
    renderHistoryJobFilter();
    renderHistory();
    renderServerInfo();
    watchRunning();
  }

  function renderStats() {
    const info = state.info;
    const total = info ? Number(info.job_count) || 0 : null;
    const active = info ? Number(info.active_count) || 0 : 0;
    el.statTotal.textContent = total === null ? "—" : String(total);
    el.statActive.textContent = total === null ? "—" : String(active);
    el.statRecurring.textContent = total === null ? "—" : String(Number(info.recurring_count) || 0);
    el.statToday.textContent = total === null ? "—" : String(Number(info.executed_today_count) || 0);
    el.navActiveCount.hidden = active === 0;
    el.navActiveCount.textContent = String(active);
  }

  function resultBadge(job) {
    const meta = RESULT_META[job.last_result];
    if (!meta) return "";
    return `<span class="result-badge ${meta.className}"><svg aria-hidden="true"><use href="${meta.icon}"/></svg>${meta.label}</span>`;
  }

  function renderTasks() {
    $$(".sort-ind").forEach(indicator => {
      indicator.textContent = indicator.dataset.sortInd === state.sort.key
        ? (state.sort.dir === 1 ? "▲" : "▼")
        : "↕";
      indicator.style.opacity = indicator.dataset.sortInd === state.sort.key ? "1" : "0.35";
    });

    const { total, offset, q, status } = state.jobsPage;
    const filtering = Boolean(q) || (status && status !== "todas");
    if (!total) {
      if (!getToken()) {
        el.tasksSummary.textContent = "Conecte-se à API para carregar suas tarefas.";
        el.jobsBody.innerHTML = `<tr><td colspan="5">
          <div class="empty-state">
            <div class="empty-icon" aria-hidden="true"><svg><use href="#i-tasks"/></svg></div>
            <h3>Tudo pronto para começar</h3>
            <p>Informe o token da API e crie sua primeira automação.</p>
            <button class="button" type="button" data-action="create"><svg aria-hidden="true"><use href="#i-plus"/></svg>Criar tarefa</button>
          </div>
        </td></tr>`;
      } else if (filtering) {
        el.tasksSummary.textContent = "Nenhuma tarefa corresponde aos filtros atuais.";
        el.jobsBody.innerHTML = `<tr><td class="table-message" colspan="5">Tente limpar a busca ou mudar o filtro de status.</td></tr>`;
      } else {
        el.tasksSummary.textContent = "Nenhuma tarefa encontrada.";
        el.jobsBody.innerHTML = `<tr><td colspan="5">
          <div class="empty-state">
            <div class="empty-icon" aria-hidden="true"><svg><use href="#i-tasks"/></svg></div>
            <h3>Nenhuma tarefa por aqui</h3>
            <p>Crie sua primeira tarefa para automatizar uma rotina.</p>
            <button class="button" type="button" data-action="create"><svg aria-hidden="true"><use href="#i-plus"/></svg>Criar tarefa</button>
          </div>
        </td></tr>`;
      }
      renderJobsPager();
      return;
    }

    const start = state.jobs.length ? offset + 1 : 0;
    const end = offset + state.jobs.length;
    el.tasksSummary.textContent = total === state.jobs.length
      ? `${total} tarefa${total === 1 ? "" : "s"} no workspace.`
      : `Mostrando ${start}–${end} de ${total} tarefas.`;

    if (!state.jobs.length) {
      el.jobsBody.innerHTML = `<tr><td class="table-message" colspan="5">Nenhuma tarefa nesta página.</td></tr>`;
      renderJobsPager();
      return;
    }

    el.jobsBody.innerHTML = state.jobs.map((job, index) => {
      const running = state.running.has(job.name);
      const statusClass = running ? "running" : job.status === "ativo" ? "active" : "inactive";
      const statusLabel = running ? "Executando" : job.status === "ativo" ? "Ativa" : "Desativa";
      const toggleLabel = job.status === "ativo" ? "Desativar" : "Ativar";
      const lastRun = job.last_run
        ? `<span class="last-run-time" title="${escapeHtml(job.last_run)}">${escapeHtml(timeAgo(job.last_run))}</span>${resultBadge(job)}`
        : `<span class="last-run-time" style="color:var(--faint)">Nunca executada</span>`;
      return `<tr style="--i:${index}">
        <td>
          <div class="job-title">${escapeHtml(job.name)}</div>
          <div class="job-description" title="${escapeHtml(job.description)}">${escapeHtml(job.description || "Sem descrição")}</div>
        </td>
        <td>
          <div class="schedule-cell">
            <code class="schedule-code" title="${escapeHtml(job.datetime)}">${escapeHtml(job.datetime)}</code>
            <span class="schedule-human">${escapeHtml(describeSchedule(job.datetime))}</span>
          </div>
        </td>
        <td><span class="status ${statusClass}">${statusLabel}</span></td>
        <td><div class="last-run">${lastRun}</div></td>
        <td>
          <div class="actions">
            <button class="icon-button${running ? " running" : ""}" type="button" data-action="run" data-name="${escapeHtml(job.name)}"
              title="${running ? "Executando…" : "Executar agora"}" aria-label="Executar ${escapeHtml(job.name)}" ${running ? "disabled" : ""}>
              <svg aria-hidden="true"><use href="${running ? "#i-refresh" : "#i-play"}"/></svg>
            </button>
            <button class="icon-button" type="button" data-action="logs" data-name="${escapeHtml(job.name)}" title="Ver log" aria-label="Ver log de ${escapeHtml(job.name)}">
              <svg aria-hidden="true"><use href="#i-file"/></svg>
            </button>
            <button class="icon-button" type="button" data-action="edit" data-name="${escapeHtml(job.name)}" title="Editar" aria-label="Editar ${escapeHtml(job.name)}">
              <svg aria-hidden="true"><use href="#i-edit"/></svg>
            </button>
            <button class="icon-button" type="button" data-action="duplicate" data-name="${escapeHtml(job.name)}" title="Duplicar" aria-label="Duplicar ${escapeHtml(job.name)}">
              <svg aria-hidden="true"><use href="#i-copy"/></svg>
            </button>
            <button class="icon-button" type="button" data-action="toggle" data-name="${escapeHtml(job.name)}" title="${toggleLabel} tarefa" aria-label="${toggleLabel} ${escapeHtml(job.name)}">
              <svg aria-hidden="true"><use href="${job.status === "ativo" ? "#i-pause" : "#i-play"}"/></svg>
            </button>
            <button class="icon-button danger" type="button" data-action="delete" data-name="${escapeHtml(job.name)}" title="Excluir" aria-label="Excluir ${escapeHtml(job.name)}">
              <svg aria-hidden="true"><use href="#i-trash"/></svg>
            </button>
          </div>
        </td>
      </tr>`;
    }).join("");
  }

  function renderJobsPager() {
    const { total, offset, limit } = state.jobsPage;
    const start = state.jobs.length ? offset + 1 : 0;
    const end = offset + state.jobs.length;
    el.jobsPage.textContent = `${start}–${end} de ${total}`;
    el.jobsPrev.disabled = offset <= 0;
    el.jobsNext.disabled = offset + limit >= total;
  }

  async function loadPickerJobs() {
    if (!getToken()) {
      state.pickerJobs = [];
      renderLogJobList();
      renderHistoryJobFilter();
      return;
    }
    try {
      const allJobs = await api("/api/jobs");
      state.pickerJobs = Array.isArray(allJobs) ? allJobs : [];
    } catch (_) {
      /* mantém o picker anterior */
    }
    renderLogJobList();
    renderHistoryJobFilter();
  }

  function renderLogJobList() {
    const query = el.logJobSearch.value.trim().toLocaleLowerCase("pt-BR");
    const allJobs = state.pickerJobs.length ? state.pickerJobs : state.jobs;
    const jobs = allJobs.filter(job => `${job.name} ${job.description}`.toLocaleLowerCase("pt-BR").includes(query));
    if (!jobs.length) {
      el.logJobList.innerHTML = `<p class="table-message">${getToken() ? "Nenhuma tarefa encontrada." : "Conecte-se para ver os logs."}</p>`;
      return;
    }
    el.logJobList.innerHTML = jobs.map(job => {
      const running = state.running.has(job.name);
      const active = state.logs.job === job.name;
      const subtitle = running
        ? "executando agora"
        : job.last_run ? `última execução ${timeAgo(job.last_run)}` : "nunca executada";
      return `<button class="log-job${active ? " active" : ""}${running ? " running" : ""}" type="button" data-name="${escapeHtml(job.name)}">
        <span class="log-job-dot" aria-hidden="true"></span>
        <span>
          <span class="log-job-name">${escapeHtml(job.name)}</span>
          <span class="log-job-sub">${escapeHtml(subtitle)}</span>
        </span>
      </button>`;
    }).join("");
  }

  function renderServerInfo() {
    const info = state.info;
    if (!info || !getToken()) {
      el.serverInfo.innerHTML = `<div class="info-empty">Conecte-se para ver as informações do servidor.</div>`;
      return;
    }
    const running = (info.running || []).length;
    const items = [
      ["Versão", `v${info.version}`],
      ["Endereço", `${info.bind_address}:${info.port}`],
      ["TLS nativo", info.native_tls ? "sim" : "não"],
      ["TLS via proxy", info.tls_terminated_by_proxy ? "sim" : "não"],
      ["X-Forwarded-For", info.trust_proxy_headers ? "confiado" : "ignorado"],
      ["Proxies confiáveis", info.trusted_proxy_ips?.length ? info.trusted_proxy_ips.join(", ") : "somente loopback"],
      ["Métricas", "/metrics (loopback ou token)"],
      ["Backup", info.backup_enabled ? `${info.backup_dir} · a cada ${info.backup_interval_seconds}s · ${info.backup_retention} cópias` : "desativado"],
      ["Backup remoto", info.backup_remote || "desativado"],
      ["Backup criptografado", info.backup_encryption ? "sim" : "não"],
      ["Última verificação", info.backup_last_verify
        ? `${info.backup_last_verify.ok}/${info.backup_last_verify.checked} OK · ${formatHistoryTime(info.backup_last_verify.timestamp)}`
        : "nunca"],
      ["Alertas de backup", info.backup_alert_enabled ? "ativos" : "desativados"],
      ["Relatório de saúde", info.backup_health_report_enabled
        ? `a cada ${info.backup_health_report_interval_seconds}s`
        : "desativado"],
      ["Tarefas cadastradas", String(info.job_count)],
      ["Tarefas ativas", String(info.active_count)],
      ["Executando agora", running ? `${running} (${info.running.join(", ")})` : "nenhuma"],
      ["Verificação de agenda", `${info.poll_interval_seconds}s`],
      ["Timeout padrão", `${info.command_timeout_seconds}s`],
      ["Timeout de webhook", `${info.notification_timeout_seconds}s`],
      ["Jobs simultâneos", String(info.max_concurrent_jobs ?? "—")],
      ["Rotação de log", info.log_max_bytes ? formatBytes(info.log_max_bytes) : "desativada"],
      ["Rate limit", info.rate_limit_per_minute ? `${info.rate_limit_per_minute}/min por IP` : "desativado"],
      ["Bloqueio de auth", info.auth_failure_limit ? `${info.auth_failure_limit} falhas · ${info.auth_lockout_seconds}s` : "desativado"],
      ["Diretório de tarefas", info.jobs_dir],
      ["Diretório de logs", info.logs_dir],
      ["Horário do servidor", new Date(info.now).toLocaleString("pt-BR")]
    ];
    el.serverInfo.innerHTML = items.map(([label, value]) => `
      <div class="info-item">
        <span class="info-label">${escapeHtml(label)}</span>
        <span class="info-value">${escapeHtml(value)}</span>
      </div>`).join("");
  }

  /* ---------------------------------------------------------------------- */
  /* Histórico                                                              */
  /* ---------------------------------------------------------------------- */

  function formatHistoryTime(value) {
    if (!value) return "—";
    const date = new Date(value);
    if (Number.isNaN(date.getTime())) return String(value ?? "—");
    return `${date.getFullYear()}-${pad2(date.getMonth() + 1)}-${pad2(date.getDate())} ${pad2(date.getHours())}:${pad2(date.getMinutes())}:${pad2(date.getSeconds())}`;
  }

  function formatDurationMs(value) {
    const ms = Number(value);
    if (!Number.isFinite(ms) || ms < 0) return "—";
    if (ms < 1000) return `${Math.round(ms)} ms`;
    const seconds = ms / 1000;
    if (seconds < 60) return `${seconds.toFixed(seconds < 10 ? 2 : 1)} s`;
    const minutes = Math.floor(seconds / 60);
    return `${minutes}m ${Math.round(seconds % 60)}s`;
  }

  function renderHistoryJobFilter() {
    const current = state.history.job;
    const allJobs = state.pickerJobs.length ? state.pickerJobs : state.jobs;
    const options = [`<option value="">Todas as tarefas</option>`]
      .concat(allJobs.map(job => `<option value="${escapeHtml(job.name)}">${escapeHtml(job.name)}</option>`));
    el.historyJobFilter.innerHTML = options.join("");
    if (current && allJobs.some(job => job.name === current)) {
      el.historyJobFilter.value = current;
    } else if (current) {
      state.history.job = "";
    }
  }

  function renderHistory() {
    const { items, total, offset, limit, enabled } = state.history;
    el.historySummary.textContent = enabled
      ? `${total} tentativa${total === 1 ? "" : "s"} registrada${total === 1 ? "" : "s"}.`
      : "Histórico desativado na configuração do servidor.";
    if (!items.length) {
      const message = !getToken()
        ? "Conecte-se à API para carregar o histórico."
        : enabled ? "Nenhuma execução encontrada para os filtros atuais." : "Histórico persistente desativado.";
      el.historyBody.innerHTML = `<tr><td class="table-message" colspan="7">${escapeHtml(message)}</td></tr>`;
    } else {
      el.historyBody.innerHTML = items.map((record, index) => `
        <tr style="--i:${index}">
          <td>${escapeHtml(formatHistoryTime(record.timestamp))}</td>
          <td><code>${escapeHtml(record.job)}</code></td>
          <td>${resultBadge({ last_result: record.result })}</td>
          <td>${record.attempt}/${record.max_attempts}</td>
          <td>${escapeHtml(formatDurationMs(record.duration_ms))}</td>
          <td>${record.exit_code ?? "—"}</td>
          <td>${record.origin === "scheduler" ? "agenda" : "manual"}</td>
        </tr>`).join("");
    }
    const start = items.length ? offset + 1 : 0;
    const end = Math.min(offset + items.length, total);
    el.historyPage.textContent = `${start}–${end} de ${total}`;
    el.historyPrev.disabled = offset <= 0;
    el.historyNext.disabled = offset + limit >= total;
  }

  async function loadHistory({ reset = false, silent = true } = {}) {
    if (!getToken()) {
      state.history.items = [];
      state.history.total = 0;
      renderHistory();
      return;
    }
    if (reset) state.history.offset = 0;
    const params = new URLSearchParams({
      limit: String(state.history.limit),
      offset: String(state.history.offset)
    });
    if (state.history.job) params.set("job", state.history.job);
    if (state.history.result) params.set("result", state.history.result);
    try {
      const data = await api(`/api/history?${params.toString()}`);
      state.history.items = Array.isArray(data.items) ? data.items : [];
      state.history.total = Number(data.total) || 0;
      state.history.limit = Number(data.limit) || state.history.limit;
      state.history.offset = Number(data.offset) || 0;
      state.history.enabled = data.enabled !== false;
      renderHistory();
    } catch (error) {
      if (!silent) toast(error.message, "error");
      renderHistory();
    }
  }

  /* ---------------------------------------------------------------------- */
  /* Navegação                                                              */
  /* ---------------------------------------------------------------------- */

  function viewFromHash() {
    const match = location.hash.match(/^#\/([a-z-]+)/);
    return match && VIEW_META[match[1]] ? match[1] : "tarefas";
  }

  function setView(name, push = false) {
    if (!VIEW_META[name]) name = "tarefas";
    if (push && viewFromHash() !== name) {
      location.hash = `#/${name}`;
      return;
    }
    state.view = name;
    $$(".view").forEach(view => view.classList.toggle("active", view.id === `view-${name}`));
    $$(".nav-link").forEach(link => link.classList.toggle("active", link.dataset.view === name));
    el.viewTitle.textContent = VIEW_META[name].title;
    el.viewSubtitle.textContent = VIEW_META[name].subtitle;
    el.newJobButton.hidden = name === "config";
    closeNav();
    if (name === "logs") {
      renderLogJobList();
      const first = state.pickerJobs[0] || state.jobs[0];
      if (!state.logs.job && first) selectLog(first.name);
      else if (state.logs.job) startLogPolling();
    } else {
      stopLogPolling();
    }
    if (name === "config") {
      renderServerInfo();
      loadBackups().catch(() => { /* mantém a última visão */ });
      loadBackupVerifications().catch(() => { /* mantém a última visão */ });
      loadBackupHealth().catch(() => { /* mantém a última visão */ });
    }
    if (name === "historico") {
      renderHistoryJobFilter();
      loadHistory({ reset: true }).catch(() => { /* mantém a última visão */ });
    }
    if (name === "logs" || name === "historico") {
      loadPickerJobs().catch(() => { /* mantém o picker anterior */ });
    }
  }

  function openNav() {
    document.body.classList.add("nav-open");
  }

  function closeNav() {
    document.body.classList.remove("nav-open");
  }

  /* ---------------------------------------------------------------------- */
  /* Diálogo de tarefa                                                      */
  /* ---------------------------------------------------------------------- */

  const scheduleHelp = {
    daily: "Escolha o horário em que a tarefa deve rodar todos os dias.",
    times: "Informe vários horários separados por vírgula; todos devem compartilhar o mesmo minuto (ex: 08:30, 12:30, 18:30).",
    interval: "Escolha de quantos em quantos minutos ou horas. Opcionalmente restrinja a um período do dia.",
    weekly: "Escolha um ou mais dias da semana e o horário da execução.",
    monthly: "Escolha o dia do mês e o horário. Dias 29-31 não rodam nos meses sem esse dia.",
    once: "A tarefa será desativada depois da tentativa de execução.",
    advanced: "Use cron de cinco campos: minuto hora dia mês dia-da-semana."
  };

  function checkWeekdays(values) {
    const selected = new Set(values.map(String));
    WEEKDAY_IDS.forEach((id, index) => {
      const box = $(`#${id}`);
      if (box) box.checked = selected.has(String(index));
    });
  }

  function updateSchedulePreview() {
    el.schedulePreview.hidden = true;
    let schedule;
    try {
      schedule = buildSchedule();
    } catch (_) {
      return;
    }
    const type = el.scheduleType.value;
    const next = nextRunEstimate(schedule);
    if (type === "once") {
      el.schedulePreview.innerHTML = `<strong>Uma vez em</strong> ${escapeHtml(formatDateTime(next) || schedule)}`;
    } else {
      el.schedulePreview.innerHTML = `<strong>Expressão:</strong> ${escapeHtml(schedule)}` +
        (next ? ` · <strong>próxima:</strong> ${escapeHtml(formatDateTime(next))}` : "");
    }
    el.schedulePreview.hidden = false;
  }

  function setScheduleForm(type, values = {}) {
    el.scheduleType.value = type;
    el.scheduleHelp.textContent = scheduleHelp[type] || "";
    const time = values.time || "08:00";
    const fields = el.scheduleFields;

    if (type === "once") {
      fields.innerHTML = `
        <div class="field"><label for="scheduleDate">Data</label><input class="input" id="scheduleDate" type="date" required value="${escapeHtml(values.date || "")}"></div>
        <div class="field"><label for="scheduleTime">Horário</label><input class="input" id="scheduleTime" type="time" required value="${escapeHtml(time)}"></div>`;
    } else if (type === "times") {
      fields.innerHTML = `
        <div class="field" style="grid-column:1/-1"><label for="scheduleTimes">Horários</label>
          <input class="input" id="scheduleTimes" type="text" required spellcheck="false" placeholder="08:30, 12:30, 18:30" value="${escapeHtml(values.times || "")}">
        </div>`;
    } else if (type === "interval") {
      const hourOptions = Array.from({ length: 24 }, (_, hour) => `<option value="${hour}">${pad2(hour)}h</option>`).join("");
      fields.innerHTML = `
        <div class="field"><label for="scheduleEvery">Executar a cada</label><input class="input" id="scheduleEvery" type="number" min="1" max="59" required value="${escapeHtml(values.every || "15")}"></div>
        <div class="field"><label for="scheduleUnit">Unidade</label><select class="select" id="scheduleUnit"><option value="minutes">Minutos</option><option value="hours">Horas</option></select></div>
        <div class="field" style="grid-column:1/-1">
          <label for="schedulePeriod">Período do dia</label>
          <div style="display:grid;grid-template-columns:1.3fr 1fr 1fr;gap:10px">
            <select class="select" id="schedulePeriod"><option value="all">Todo o dia</option><option value="limited">Entre horas</option></select>
            <select class="select" id="schedulePeriodStart" disabled>${hourOptions}</select>
            <select class="select" id="schedulePeriodEnd" disabled>${hourOptions}</select>
          </div>
        </div>`;
      $("#scheduleUnit").value = values.unit || "minutes";
      $("#schedulePeriod").value = values.period || "all";
      $("#schedulePeriodStart").value = values.periodStart ?? "8";
      $("#schedulePeriodEnd").value = values.periodEnd ?? "18";
      const togglePeriod = () => {
        const limited = $("#schedulePeriod").value === "limited";
        $("#schedulePeriodStart").disabled = !limited;
        $("#schedulePeriodEnd").disabled = !limited;
      };
      togglePeriod();
      $("#scheduleEvery").max = values.unit === "hours" ? "23" : "59";
      $("#scheduleUnit").addEventListener("change", event => {
        $("#scheduleEvery").max = event.target.value === "hours" ? "23" : "59";
        updateSchedulePreview();
      });
      $("#schedulePeriod").addEventListener("change", togglePeriod);
    } else if (type === "daily") {
      fields.innerHTML = `<div class="field" style="grid-column:1/-1"><label for="scheduleTime">Horário</label><input class="input" id="scheduleTime" type="time" required value="${escapeHtml(time)}"></div>`;
    } else if (type === "weekly") {
      fields.innerHTML = `
        <div class="field" style="grid-column:1/-1">
          <span>Dias da semana</span>
          <div class="weekday-checks" id="weekdayChecks">${WEEKDAY_LABELS.map(([value, label]) => `<label><input type="checkbox" id="wd${value}" value="${value}"> ${label}</label>`).join("")}</div>
          <div class="weekday-quick" id="weekdayQuick">
            <button type="button" data-quick="1-5">Dias úteis</button>
            <button type="button" data-quick="0,6">Fim de semana</button>
            <button type="button" data-quick="0-6">Todos</button>
          </div>
        </div>
        <div class="field" style="grid-column:1/-1"><label for="scheduleTime">Horário</label><input class="input" id="scheduleTime" type="time" required value="${escapeHtml(time)}"></div>`;
      checkWeekdays(values.weekdays || ["1"]);
      $("#weekdayChecks").addEventListener("change", updateSchedulePreview);
      $("#weekdayQuick").addEventListener("click", event => {
        const button = event.target.closest("button[data-quick]");
        if (!button) return;
        const range = button.dataset.quick;
        const [start, end] = range.includes("-") ? range.split("-").map(Number) : [Number(range), Number(range)];
        const picked = [];
        for (let day = start; day <= end; day++) picked.push(String(day));
        checkWeekdays(picked);
        updateSchedulePreview();
      });
    } else if (type === "monthly") {
      fields.innerHTML = `
        <div class="field"><label for="scheduleDay">Dia do mês</label><input class="input" id="scheduleDay" type="number" min="1" max="31" required value="${escapeHtml(values.day || "1")}"></div>
        <div class="field"><label for="scheduleTime">Horário</label><input class="input" id="scheduleTime" type="time" required value="${escapeHtml(time)}"></div>`;
    } else {
      fields.innerHTML = `<div class="field" style="grid-column:1/-1"><label for="scheduleCron">Expressão cron</label><input class="input" id="scheduleCron" required spellcheck="false" placeholder="*/15 * * * *" value="${escapeHtml(values.cron || "")}"></div>`;
    }
    updateSchedulePreview();
  }

  function setScheduleFromValue(value) {
    const fields = String(value).trim().split(/\s+/);
    if (fields.length === 5) {
      const [minute, hour, day, month, weekday] = fields;
      let match;

      if (day === "*" && month === "*" && weekday === "*") {
        // Intervalo simples: "*/N * * * *" ou "0 */N * * *"
        if (hour === "*" && (minute === "*" || (match = minute.match(/^\*\/(\d+)$/)))) {
          setScheduleForm("interval", { every: match ? match[1] : "1", unit: "minutes" });
          return;
        }
        if (minute === "0" && (hour === "*" || (match = hour.match(/^\*\/(\d+)$/)))) {
          setScheduleForm("interval", { every: match ? match[1] : "1", unit: "hours" });
          return;
        }
        // Intervalo com período: "*/N H1-H2 * * *" ou "0 H1-H2/N * * *"
        match = minute.match(/^\*\/(\d+)$/);
        if (match && /^\d+-\d+$/.test(hour)) {
          setScheduleForm("interval", {
            every: match[1], unit: "minutes", period: "limited",
            periodStart: String(hour.split("-")[0]), periodEnd: String(hour.split("-")[1])
          });
          return;
        }
        match = hour.match(/^(\d+)-(\d+)\/(\d+)$/);
        if (minute === "0" && match) {
          setScheduleForm("interval", {
            every: match[3], unit: "hours", period: "limited",
            periodStart: match[1], periodEnd: match[2]
          });
          return;
        }
        // Vários horários por dia: minuto numérico comum e lista de horas
        if (/^\d+$/.test(minute) && /^[\d,]+$/.test(hour) && hour.includes(",")) {
          const hours = hour.split(",");
          if (hours.every(item => /^\d+$/.test(item))) {
            setScheduleForm("times", {
              times: hours.map(item => `${pad2(item)}:${pad2(minute)}`).join(", ")
            });
            return;
          }
        }
      }

      if (/^\d+$/.test(minute) && /^\d+$/.test(hour) && day === "*" && month === "*" && weekday === "*") {
        setScheduleForm("daily", { time: `${pad2(hour)}:${pad2(minute)}` });
        return;
      }

      if (/^\d+$/.test(minute) && /^\d+$/.test(hour) && day === "*" && month === "*" && /^\d$/.test(weekday)) {
        setScheduleForm("weekly", { time: `${pad2(hour)}:${pad2(minute)}`, weekdays: [weekday === "7" ? "0" : weekday] });
        return;
      }

      if (/^\d+$/.test(minute) && /^\d+$/.test(hour) && day === "*" && month === "*"
        && /^[0-7,\-]+$/.test(weekday) && (weekday.includes(",") || weekday.includes("-"))) {
        const picked = new Set();
        for (const part of weekday.split(",")) {
          if (part.includes("-")) {
            const [start, end] = part.split("-").map(Number);
            if (!Number.isInteger(start) || !Number.isInteger(end) || start > end || start < 0 || end > 7) {
              setScheduleForm("advanced", { cron: value });
              return;
            }
            for (let dayOfWeek = start; dayOfWeek <= end; dayOfWeek++) picked.add(String(dayOfWeek === 7 ? 0 : dayOfWeek));
          } else {
            const dayOfWeek = Number(part);
            if (!Number.isInteger(dayOfWeek) || dayOfWeek < 0 || dayOfWeek > 7) {
              setScheduleForm("advanced", { cron: value });
              return;
            }
            picked.add(String(dayOfWeek === 7 ? 0 : dayOfWeek));
          }
        }
        const ordered = [0, 1, 2, 3, 4, 5, 6].filter(dayOfWeek => picked.has(String(dayOfWeek)));
        if (!ordered.length) {
          setScheduleForm("advanced", { cron: value });
          return;
        }
        setScheduleForm("weekly", { time: `${pad2(hour)}:${pad2(minute)}`, weekdays: ordered });
        return;
      }

      if (/^\d+$/.test(minute) && /^\d+$/.test(hour) && /^\d+$/.test(day) && month === "*" && weekday === "*") {
        setScheduleForm("monthly", { time: `${pad2(hour)}:${pad2(minute)}`, day });
        return;
      }

      const once = String(value).match(/^(\d{1,2}) (\d{1,2}) (\d{4}) (\d{1,2}) (\d{1,2})$/);
      if (once) {
        const [, dayOnce, monthOnce, year, hourOnce, minuteOnce] = once;
        setScheduleForm("once", {
          date: `${year}-${pad2(monthOnce)}-${pad2(dayOnce)}`,
          time: `${pad2(hourOnce)}:${pad2(minuteOnce)}`
        });
        return;
      }
      setScheduleForm("advanced", { cron: value });
      return;
    }

    const once = parseOnceDate(value);
    if (once) {
      setScheduleForm("once", {
        date: `${once.getFullYear()}-${pad2(once.getMonth() + 1)}-${pad2(once.getDate())}`,
        time: `${pad2(once.getHours())}:${pad2(once.getMinutes())}`
      });
      return;
    }
    setScheduleForm("advanced", { cron: value });
  }

  function buildSchedule() {
    const type = el.scheduleType.value;
    if (type === "advanced") {
      const cron = $("#scheduleCron").value.trim();
      if (!cron) throw new Error("Informe a expressão cron.");
      return cron;
    }
    if (type === "once") {
      const date = $("#scheduleDate").value;
      const time = $("#scheduleTime").value;
      if (!date || !time) throw new Error("Informe a data e o horário.");
      const [year, month, day] = date.split("-");
      const [hour, minute] = time.split(":");
      return `${day} ${month} ${year} ${hour} ${minute}`;
    }
    if (type === "times") {
      const raw = $("#scheduleTimes").value;
      const entries = raw.split(",").map(entry => entry.trim()).filter(Boolean);
      if (!entries.length) throw new Error("Informe pelo menos um horário (ex: 08:30).");
      const parsed = entries.map(entry => {
        const match = entry.match(/^(\d{1,2}):(\d{2})$/);
        if (!match) throw new Error(`Horário inválido: "${entry}". Use HH:MM.`);
        const hour = Number(match[1]);
        const minute = Number(match[2]);
        if (hour > 23 || minute > 59) throw new Error(`Horário inválido: "${entry}".`);
        return { hour, minute };
      });
      const minutes = new Set(parsed.map(entry => entry.minute));
      if (minutes.size !== 1) {
        throw new Error("Todos os horários devem compartilhar o mesmo minuto (ex: 08:30, 12:30).");
      }
      const hours = [...new Set(parsed.map(entry => entry.hour))].sort((left, right) => left - right);
      return `${parsed[0].minute} ${hours.join(",")} * * *`;
    }

    if (type === "interval") {
      const every = Number($("#scheduleEvery").value);
      const unit = $("#scheduleUnit").value;
      const maximum = unit === "hours" ? 23 : 59;
      if (!Number.isInteger(every) || every < 1 || every > maximum) {
        throw new Error(`O intervalo deve ser entre 1 e ${maximum}.`);
      }
      if ($("#schedulePeriod").value === "limited") {
        const start = Number($("#schedulePeriodStart").value);
        const end = Number($("#schedulePeriodEnd").value);
        if (!Number.isInteger(start) || !Number.isInteger(end) || start > end) {
          throw new Error("O período deve ter horário inicial menor ou igual ao final.");
        }
        return unit === "hours"
          ? `0 ${start}-${end}/${every} * * *`
          : `*/${every} ${start}-${end} * * *`;
      }
      return unit === "hours"
        ? `0 */${every} * * *`
        : every === 1 ? "* * * * *" : `*/${every} * * * *`;
    }

    const time = $("#scheduleTime").value;
    if (!time) throw new Error("Informe o horário da tarefa.");
    const [hour, minute] = time.split(":");
    if (type === "daily") return `${minute} ${hour} * * *`;
    if (type === "weekly") {
      const picked = WEEKDAY_LABELS
        .map(([value]) => value)
        .filter(value => $(`#wd${value}`)?.checked);
      if (!picked.length) throw new Error("Selecione pelo menos um dia da semana.");
      return `${minute} ${hour} * * ${picked.join(",")}`;
    }
    const day = Number($("#scheduleDay").value);
    if (!Number.isInteger(day) || day < 1 || day > 31) {
      throw new Error("O dia do mês deve estar entre 1 e 31.");
    }
    return `${minute} ${hour} ${day} * *`;
  }

  function uniqueDuplicateName(baseName) {
    const candidates = [baseName, ...Array.from({ length: 20 }, (_, index) => `${baseName}-${index + 2}`)];
    for (const candidate of candidates) {
      if (candidate.length <= 64 && !state.jobs.some(job => job.name === candidate)) return candidate;
    }
    return null;
  }

  function resetJobForm() {
    el.jobForm.reset();
    el.jobStatus.value = "ativo";
    el.jobWorkingDir.value = "";
    el.jobTimeout.value = "";
    el.jobRetries.value = "";
    el.jobRetryDelay.value = "";
    el.jobNotifyUrl.value = "";
    el.jobName.disabled = false;
    state.editingVersion = null;
  }

  function openCreateDialog() {
    if (!getToken()) {
      toast("Conecte-se à API antes de criar tarefas.", "error");
      setView("config");
      return;
    }
    state.editing = null;
    resetJobForm();
    el.formTitle.textContent = "Nova tarefa";
    setScheduleForm("daily", { time: "08:00" });
    el.saveJobButton.textContent = "Criar tarefa";
    el.jobDialog.showModal();
  }

  function openEditDialog(job) {
    state.editing = job.name;
    resetJobForm();
    state.editingVersion = job.version || 1;
    el.formTitle.textContent = "Editar tarefa";
    el.jobName.value = job.name;
    el.jobName.disabled = true;
    el.jobDescription.value = job.description;
    el.jobStatus.value = job.status;
    setScheduleFromValue(job.datetime);
    el.jobAction.value = job.action;
    el.jobWorkingDir.value = job.working_dir || "";
    el.jobTimeout.value = job.timeout_seconds || "";
    el.jobRetries.value = job.retries || "";
    el.jobRetryDelay.value = job.retry_delay_seconds || "";
    el.jobNotifyUrl.value = job.notify_url || "";
    el.saveJobButton.textContent = "Salvar alterações";
    el.jobDialog.showModal();
  }

  function openDuplicateDialog(job) {
    if (!getToken()) return;
    const name = uniqueDuplicateName(job.name);
    if (!name) {
      toast("Não foi possível gerar um nome livre para a cópia.", "error");
      return;
    }
    state.editing = null;
    resetJobForm();
    el.formTitle.textContent = "Duplicar tarefa";
    el.jobName.value = name;
    el.jobDescription.value = job.description;
    el.jobStatus.value = job.status;
    setScheduleFromValue(job.datetime);
    el.jobAction.value = job.action;
    el.jobWorkingDir.value = job.working_dir || "";
    el.jobTimeout.value = job.timeout_seconds || "";
    el.jobRetries.value = job.retries || "";
    el.jobRetryDelay.value = job.retry_delay_seconds || "";
    el.jobNotifyUrl.value = job.notify_url || "";
    el.saveJobButton.textContent = "Criar tarefa";
    el.jobDialog.showModal();
    toast(`Cópia preparada como "${name}". Revise e salve.`, "info");
  }

  async function saveJob(event) {
    event.preventDefault();
    const button = el.saveJobButton;
    button.disabled = true;
    try {
      const workingDir = el.jobWorkingDir.value.trim();
      const timeoutValue = el.jobTimeout.value;
      let timeoutSeconds = 0;
      if (timeoutValue !== "") {
        timeoutSeconds = Number(timeoutValue);
        if (!Number.isInteger(timeoutSeconds) || timeoutSeconds < 1 || timeoutSeconds > 86400) {
          throw new Error("O tempo limite deve ser entre 1 e 86.400 segundos.");
        }
      }
      const retriesValue = el.jobRetries.value;
      let retries = 0;
      if (retriesValue !== "") {
        retries = Number(retriesValue);
        if (!Number.isInteger(retries) || retries < 0 || retries > 10) {
          throw new Error("As tentativas extras devem ser entre 0 e 10.");
        }
      }
      const retryDelayValue = el.jobRetryDelay.value;
      let retryDelay = 0;
      if (retryDelayValue !== "") {
        retryDelay = Number(retryDelayValue);
        if (!Number.isInteger(retryDelay) || retryDelay < 1 || retryDelay > 3600) {
          throw new Error("O atraso entre tentativas deve ser entre 1 e 3.600 segundos.");
        }
      }
      const notifyUrl = el.jobNotifyUrl.value.trim();
      if (notifyUrl) {
        let parsed;
        try { parsed = new URL(notifyUrl); } catch (_) { parsed = null; }
        if (!parsed || !["http:", "https:"].includes(parsed.protocol)) {
          throw new Error("O webhook deve ser uma URL http ou https válida.");
        }
      }
      const payload = {
        description: el.jobDescription.value.trim(),
        datetime: buildSchedule(),
        action: el.jobAction.value.trim(),
        status: el.jobStatus.value,
        working_dir: workingDir,
        timeout_seconds: timeoutSeconds,
        retries: retries,
        retry_delay_seconds: retryDelay,
        notify_url: notifyUrl
      };
      if (state.editing) {
        const headers = {};
        if (state.editingVersion) headers["If-Match"] = `"${state.editingVersion}"`;
        await api(`/api/jobs/${encodedName(state.editing)}`, { method: "PATCH", body: JSON.stringify(payload), headers });
        toast("Tarefa atualizada.", "success");
      } else {
        payload.name = el.jobName.value.trim();
        await api("/api/jobs", { method: "POST", body: JSON.stringify(payload) });
        toast("Tarefa criada.", "success");
      }
      el.jobDialog.close();
      await refreshData({ silent: true });
    } catch (error) {
      if (error.status === 409) {
        toast("A tarefa foi alterada por outro cliente. Recarregue e edite novamente.", "error");
        el.jobDialog.close();
        await refreshData({ silent: true }).catch(() => {});
      } else {
        toast(error.message, "error");
      }
    } finally {
      button.disabled = false;
    }
  }

  /* ---------------------------------------------------------------------- */
  /* Ações de tarefa                                                        */
  /* ---------------------------------------------------------------------- */

  async function loadBackupVerifications() {
    if (!getToken()) {
      el.backupVerificationsSummary.textContent = "Conecte-se para verificar o histórico.";
      el.backupVerificationsBody.innerHTML = `<tr><td class="table-message" colspan="4">Conecte-se para verificar o histórico.</td></tr>`;
      return;
    }
    try {
      const data = await api("/api/backups/verifications?limit=20");
      renderBackupVerifications(data);
    } catch (error) {
      el.backupVerificationsSummary.textContent = error.message;
      el.backupVerificationsBody.innerHTML = `<tr><td class="table-message" colspan="4">${escapeHtml(error.message)}</td></tr>`;
    }
  }

  function renderBackupVerifications(data) {
    const items = Array.isArray(data.items) ? data.items : [];
    const total = Number(data.total) || 0;
    el.backupVerificationsSummary.textContent = total
      ? `${total} verificação(ões) registradas; mostrando as ${items.length} mais recentes.`
      : "Nenhuma verificação registrada.";
    if (!items.length) {
      el.backupVerificationsBody.innerHTML = `<tr><td class="table-message" colspan="4">Nenhuma verificação registrada.</td></tr>`;
      return;
    }
    el.backupVerificationsBody.innerHTML = items.map(item => {
      const failed = Array.isArray(item.items) ? item.items.filter(entry => !entry.ok) : [];
      const details = failed.length
        ? failed.slice(0, 3).map(entry => `${entry.name}: ${entry.error || "falha"}`).join(" · ") +
          (failed.length > 3 ? ` (+${failed.length - 3})` : "")
        : "—";
      const badge = item.failed
        ? `<span class="result-badge result-error"><svg aria-hidden="true"><use href="#i-alert"/></svg>${item.failed} falha(s)</span>`
        : `<span class="result-badge result-success"><svg aria-hidden="true"><use href="#i-check"/></svg>OK</span>`;
      return `<tr>
        <td>${escapeHtml(formatHistoryTime(item.timestamp))}</td>
        <td>${item.ok}/${item.checked} ${badge}</td>
        <td>${item.failed}</td>
        <td>${escapeHtml(details)}</td>
      </tr>`;
    }).join("");
  }

  async function loadBackupHealth() {
    if (!getToken()) {
      el.backupHealthStatus.className = "result-badge";
      el.backupHealthStatus.textContent = "—";
      el.backupHealthTitle.textContent = "Status dos backups";
      el.backupHealthDetails.textContent = "Conecte-se para carregar o resumo.";
      return;
    }
    try {
      renderBackupHealth(await api("/api/backups/health"));
    } catch (error) {
      el.backupHealthStatus.className = "result-badge result-error";
      el.backupHealthStatus.textContent = "Erro";
      el.backupHealthTitle.textContent = "Não foi possível consultar os backups";
      el.backupHealthDetails.textContent = error.message;
    }
  }

  function renderBackupHealth(data) {
    const meta = {
      ok: { className: "result-success", label: "OK", title: "Backups saudáveis" },
      warning: { className: "result-timeout", label: "Atenção", title: "Atenção nos backups" },
      error: { className: "result-error", label: "Erro", title: "Falha nos backups" },
      disabled: { className: "", label: "Desativado", title: "Backups desativados" }
    }[data.status] || { className: "", label: "—", title: "Status dos backups" };
    el.backupHealthStatus.className = `result-badge ${meta.className}`.trim();
    el.backupHealthStatus.textContent = meta.label;
    el.backupHealthTitle.textContent = meta.title;
    const parts = [
      `local: ${data.local_count || 0}`,
      `remoto: ${data.remote || "desativado"}`,
      `retenção: ${data.retention ?? "—"}`,
      `último upload: ${data.last_upload ? formatHistoryTime(data.last_upload.timestamp) : "—"}`,
      `última verificação: ${data.last_verify ? formatHistoryTime(data.last_verify.timestamp) : "—"}`,
      `último relatório: ${data.last_report ? formatHistoryTime(data.last_report.timestamp) : "—"}`
    ];
    const problems = Array.isArray(data.problems) && data.problems.length
      ? ` Problemas: ${data.problems.join("; ")}.`
      : "";
    el.backupHealthDetails.textContent = `${parts.join(" · ")}.${problems}`;
  }

  async function exportBackupHealth(format) {
    const token = getToken();
    if (!token) {
      toast("Conecte-se à API para exportar.", "error");
      return;
    }
    try {
      const response = await fetch(`/api/backups/health/export?format=${encodeURIComponent(format)}`, {
        headers: { Authorization: `Bearer ${token}` }
      });
      if (!response.ok) {
        if (response.status === 401) {
          setToken("");
          setConnection("offline", "Token inválido");
        }
        throw new Error(`Erro HTTP ${response.status}`);
      }
      const blob = await response.blob();
      const disposition = response.headers.get("content-disposition") || "";
      const match = disposition.match(/filename="([^"]+)"/);
      const filename = match ? match[1] : `datatask-backup-health.${format}`;
      const url = URL.createObjectURL(blob);
      const link = document.createElement("a");
      link.href = url;
      link.download = filename;
      document.body.append(link);
      link.click();
      link.remove();
      URL.revokeObjectURL(url);
      toast(`Relatório ${format.toUpperCase()} gerado.`, "success");
    } catch (error) {
      toast(error.message, "error");
    }
  }

  async function loadBackups() {
    if (!getToken()) {
      el.backupsSummary.textContent = "Conecte-se para listar os backups.";
      el.backupsBody.innerHTML = `<tr><td class="table-message" colspan="6">Conecte-se para listar os backups.</td></tr>`;
      return;
    }
    try {
      const data = await api("/api/backups");
      renderBackups(data);
    } catch (error) {
      el.backupsSummary.textContent = error.message;
      el.backupsBody.innerHTML = `<tr><td class="table-message" colspan="6">${escapeHtml(error.message)}</td></tr>`;
    }
  }

  function renderBackups(data) {
    const items = Array.isArray(data.items) ? data.items : [];
    if (!data.enabled) {
      el.backupsSummary.textContent = "Backups automáticos desativados (configure backup_dir).";
    } else {
      el.backupsSummary.textContent = items.length
        ? `${items.length} backup(s) em ${data.dir}. Restauração usa prévia e verificação SHA-256.`
        : `Nenhum backup em ${data.dir}.`;
    }
    if (!items.length) {
      el.backupsBody.innerHTML = `<tr><td class="table-message" colspan="6">${escapeHtml(
        data.enabled ? "Nenhum backup encontrado." : "Backups automáticos desativados."
      )}</td></tr>`;
      return;
    }
    el.backupsBody.innerHTML = items.map(item => {
      const checksum = item.checksum_ok
        ? `<span class="result-badge result-success"><svg aria-hidden="true"><use href="#i-check"/></svg>OK</span>`
        : `<span class="result-badge result-error"><svg aria-hidden="true"><use href="#i-alert"/></svg>${item.sha256 ? "Divergente" : "Ausente"}</span>`;
      const disabled = !item.checksum_ok || !item.valid_json;
      return `<tr>
        <td>${escapeHtml(formatHistoryTime(item.modified))}</td>
        <td><code>${escapeHtml(item.name)}</code></td>
        <td>${item.jobs}</td>
        <td>${checksum}</td>
        <td>${escapeHtml(formatBytes(item.size))}</td>
        <td style="text-align:right">
          <button class="button outline small" type="button" data-backup="${escapeHtml(item.name)}" ${disabled ? "disabled" : ""}>
            Restaurar
          </button>
        </td>
      </tr>`;
    }).join("");
  }

  async function restoreBackup(name) {
    const token = getToken();
    if (!token) {
      toast("Conecte-se à API para restaurar.", "error");
      return;
    }
    try {
      const response = await fetch(`/api/backups/${encodeURIComponent(name)}`, {
        headers: { Authorization: `Bearer ${token}` }
      });
      if (!response.ok) {
        if (response.status === 401) {
          setToken("");
          setConnection("offline", "Token inválido");
        }
        throw new Error(`Erro HTTP ${response.status}`);
      }
      const imported = await response.json();
      await importJobsDocument(imported);
    } catch (error) {
      toast(error.message, "error");
    }
  }

  async function exportJobs() {
    const token = getToken();
    if (!token) {
      toast("Conecte-se à API para exportar.", "error");
      return;
    }
    try {
      const response = await fetch("/api/jobs/export", {
        headers: { Authorization: `Bearer ${token}` }
      });
      if (!response.ok) {
        if (response.status === 401) {
          setToken("");
          setConnection("offline", "Token inválido");
        }
        throw new Error(`Erro HTTP ${response.status}`);
      }
      const blob = await response.blob();
      const disposition = response.headers.get("content-disposition") || "";
      const match = disposition.match(/filename="([^"]+)"/);
      const filename = match ? match[1] : "datatask-jobs.json";
      const url = URL.createObjectURL(blob);
      const link = document.createElement("a");
      link.href = url;
      link.download = filename;
      document.body.append(link);
      link.click();
      link.remove();
      URL.revokeObjectURL(url);
      toast("Exportação gerada.", "success");
    } catch (error) {
      toast(error.message, "error");
    }
  }

  async function importJobsFile(file) {
    if (!file) return;
    if (file.size > 2 * 1024 * 1024) {
      toast("O arquivo de importação deve ter no máximo 2 MiB.", "error");
      return;
    }
    let imported;
    try {
      imported = JSON.parse(await file.text());
    } catch (_) {
      toast("O arquivo não é um JSON válido.", "error");
      return;
    }
    if (!imported || !Array.isArray(imported.jobs)) {
      toast("O arquivo não contém uma lista 'jobs'.", "error");
      return;
    }
    await importJobsDocument(imported);
  }

  async function importJobsDocument(imported) {
    try {
      const preview = await api("/api/jobs/import", {
        method: "POST",
        body: JSON.stringify({ ...imported, dry_run: true, on_conflict: "skip" })
      });
      const created = preview.created?.length || 0;
      const skipped = preview.skipped?.length || 0;
      let mode = "skip";
      if (skipped > 0) {
        const overwrite = await confirmAction({
          title: "Jobs existentes",
          message: `${created} novo(s) e ${skipped} já existente(s). Sobrescrever os existentes?`,
          acceptLabel: "Sobrescrever"
        });
        if (overwrite) {
          mode = "overwrite";
        } else {
          const proceed = await confirmAction({
            title: "Ignorar existentes",
            message: `Importar somente ${created} job(s) novo(s)?`,
            acceptLabel: "Importar",
            danger: false
          });
          if (!proceed) return;
          if (created === 0) {
            toast("Nenhum job novo para importar.", "info");
            return;
          }
        }
      } else {
        const proceed = await confirmAction({
          title: "Importar jobs",
          message: `Criar ${created} job(s)?`,
          acceptLabel: "Importar",
          danger: false
        });
        if (!proceed) return;
      }

      const result = await api("/api/jobs/import", {
        method: "POST",
        body: JSON.stringify({ ...imported, dry_run: false, on_conflict: mode })
      });
      el.importFile.value = "";
      toast(
        `Importação concluída: ${result.created?.length || 0} criado(s), ` +
        `${result.overwritten?.length || 0} sobrescrito(s), ` +
        `${result.skipped?.length || 0} ignorado(s).`,
        "success"
      );
      await refreshData({ silent: true });
      if (state.view === "logs" || state.view === "historico") {
        await loadPickerJobs();
      }
      if (state.view === "config") {
        await loadBackups();
        await loadBackupVerifications();
        await loadBackupHealth();
      }
    } catch (error) {
      toast(error.message, "error");
    }
  }

  function watchRunning() {
    clearInterval(state.timers.watch);
    state.timers.watch = null;
    if (!state.running.size) return;
    state.timers.watch = setInterval(() => {
      refreshData({ silent: true }).catch(() => { /* mantém a última visão */ });
      if (!state.running.size) {
        clearInterval(state.timers.watch);
        state.timers.watch = null;
      }
    }, 2000);
  }

  async function runJob(name) {
    try {
      await api(`/api/jobs/${encodedName(name)}/run`, { method: "POST" });
      state.running.add(name);
      renderTasks();
      renderLogJobList();
      toast(`"${name}" iniciada.`, "info");
      if (state.logs.job === name) {
        state.logs.live = true;
        el.logLive.checked = true;
        startLogPolling();
        pollLog();
      }
      watchRunning();
    } catch (error) {
      toast(error.message, "error");
      await refreshData({ silent: true }).catch(() => {});
    }
  }

  async function toggleJob(name) {
    const job = state.jobs.find(item => item.name === name);
    if (!job) return;
    const nextStatus = job.status === "ativo" ? "desativo" : "ativo";
    const endpoint = nextStatus === "ativo" ? "activate" : "deactivate";
    try {
      await api(`/api/jobs/${encodedName(name)}/${endpoint}`, { method: "POST" });
      toast(nextStatus === "ativo" ? "Tarefa ativada." : "Tarefa desativada.", "success");
      await refreshData({ silent: true });
    } catch (error) {
      toast(error.message, "error");
      await refreshData({ silent: true }).catch(() => {});
    }
  }

  async function deleteJob(name) {
    const confirmed = await confirmAction({
      title: "Excluir tarefa",
      message: `Excluir "${name}"? O arquivo de log será mantido no disco.`,
      acceptLabel: "Excluir"
    });
    if (!confirmed) return;
    try {
      await api(`/api/jobs/${encodedName(name)}`, { method: "DELETE" });
      toast("Tarefa excluída.", "success");
      if (state.logs.job === name) clearLogSelection();
      await refreshData({ silent: true });
    } catch (error) {
      toast(error.message, "error");
    }
  }

  async function handleJobAction(action, name) {
    const job = state.jobs.find(item => item.name === name);
    if (!job) return;
    if (action === "edit") { openEditDialog(job); return; }
    if (action === "duplicate") { openDuplicateDialog(job); return; }
    if (action === "logs") { setView("logs", true); selectLog(name); return; }
    if (action === "run") { await runJob(name); return; }
    if (action === "toggle") { await toggleJob(name); return; }
    if (action === "delete") { await deleteJob(name); return; }
  }

  /* ---------------------------------------------------------------------- */
  /* Logs                                                                   */
  /* ---------------------------------------------------------------------- */

  function clearLogSelection() {
    state.logs.job = null;
    state.logs.text = "";
    state.logs.offset = 0;
    state.logs.size = 0;
    state.logs.updatedAt = 0;
    state.logs.filter = "";
    el.logFilter.value = "";
    el.logTitle.textContent = "Selecione uma tarefa";
    el.logMeta.textContent = "Escolha uma tarefa à esquerda para acompanhar a saída.";
    el.logResult.hidden = true;
    el.livePill.hidden = true;
    setLogButtons(false);
    stopLogPolling();
    renderLog();
  }

  function updateLogMeta() {
    const job = state.jobs.find(item => item.name === state.logs.job);
    if (!job) return;
    const parts = [job.description || "Sem descrição", describeSchedule(job.datetime)];
    if (state.running.has(job.name)) parts.push("executando agora");
    el.logTitle.textContent = job.name;
    el.logMeta.textContent = parts.join(" · ");
    const meta = RESULT_META[job.last_result];
    el.logResult.hidden = !meta;
    if (meta) {
      el.logResult.className = `result-badge ${meta.className}`;
      el.logResult.innerHTML = `<svg aria-hidden="true"><use href="${meta.icon}"/></svg>${meta.label}`;
    }
    el.livePill.hidden = !(state.logs.live && state.logs.job);
  }

  function setLogButtons(enabled) {
    el.logRefresh.disabled = !enabled;
    el.logDownload.disabled = !enabled;
    el.logClear.disabled = !enabled;
  }

  function selectLog(name) {
    if (state.logs.job === name && state.logs.text) {
      updateLogMeta();
      return;
    }
    state.logs.job = name;
    state.logs.text = "";
    state.logs.offset = 0;
    state.logs.size = 0;
    state.logs.updatedAt = 0;
    setLogButtons(true);
    updateLogMeta();
    renderLogJobList();
    renderLog();
    loadLogFull();
    if (state.view === "logs") startLogPolling();
  }

  async function loadLogFull() {
    if (!state.logs.job) return;
    try {
      const data = await api(`/api/jobs/${encodedName(state.logs.job)}/logs/tail`);
      if (data.exists === false) {
        state.logs.text = "";
        state.logs.offset = 0;
        state.logs.size = 0;
      } else {
        state.logs.text = data.content || "";
        state.logs.offset = data.offset || 0;
        state.logs.size = data.size || 0;
      }
      state.logs.updatedAt = Date.now();
      renderLog();
    } catch (error) {
      state.logs.text = "";
      renderLog();
      toast(error.message, "error");
    }
  }

  async function pollLog() {
    if (!state.logs.job || !state.logs.live || document.hidden || logPolling) return;
    logPolling = true;
    try {
      const data = await api(`/api/jobs/${encodedName(state.logs.job)}/logs/tail?offset=${state.logs.offset}`);
      if (state.logs.job === null) return;
      if (data.exists === false) {
        if (state.logs.size > 0) {
          state.logs.text = "";
          state.logs.offset = 0;
          state.logs.size = 0;
          renderLog();
        }
        return;
      }
      state.logs.size = data.size || 0;
      if (data.content) {
        state.logs.text += data.content;
        if (state.logs.text.length > MAX_LOG_TEXT) {
          state.logs.text = state.logs.text.slice(-Math.floor(MAX_LOG_TEXT * 0.75));
        }
        state.logs.offset = data.offset || state.logs.offset;
        state.logs.updatedAt = Date.now();
        renderLog();
      } else if (data.offset > state.logs.offset) {
        state.logs.offset = data.offset;
      }
    } catch (error) {
      if (error.message.includes("não encontrado") || error.message.includes("not found")) {
        /* tarefa removida; a próxima atualização limpa a seleção */
      }
    } finally {
      logPolling = false;
    }
  }

  function renderLog() {
    const filter = state.logs.filter.trim().toLocaleLowerCase("pt-BR");
    let text = state.logs.text;
    if (filter) {
      text = text.split("\n").filter(line => line.toLocaleLowerCase("pt-BR").includes(filter)).join("\n");
    }
    const empty = !text.trim();
    el.logOutput.classList.toggle("empty", empty);
    el.logOutput.classList.toggle("wrap", !!prefs.wrapLogs);
    if (empty) {
      el.logOutput.textContent = filter
        ? "Nenhuma linha corresponde ao filtro."
        : state.logs.job ? "Sem conteúdo no log ainda." : "Selecione uma tarefa para visualizar o log.";
    } else {
      el.logOutput.textContent = text;
      if (state.logs.autoScroll) el.logOutput.scrollTop = el.logOutput.scrollHeight;
    }
    el.logSize.textContent = formatBytes(state.logs.size);
    el.logUpdated.textContent = state.logs.updatedAt
      ? `atualizado ${timeAgo(state.logs.updatedAt)}`
      : "—";
  }

  function startLogPolling() {
    stopLogPolling();
    if (!state.logs.job || !state.logs.live || state.view !== "logs") return;
    state.timers.log = setInterval(pollLog, Math.max(1, prefs.logPoll) * 1000);
  }

  function stopLogPolling() {
    clearInterval(state.timers.log);
    state.timers.log = null;
    logPolling = false;
  }

  async function downloadLog() {
    if (!state.logs.job) return;
    try {
      const text = await api(`/api/jobs/${encodedName(state.logs.job)}/logs?download=1`);
      const blob = new Blob([text ?? ""], { type: "text/plain;charset=utf-8" });
      const url = URL.createObjectURL(blob);
      const anchor = document.createElement("a");
      anchor.href = url;
      anchor.download = `${state.logs.job}.log`;
      document.body.append(anchor);
      anchor.click();
      anchor.remove();
      setTimeout(() => URL.revokeObjectURL(url), 1000);
      toast("Log baixado.", "success");
    } catch (error) {
      toast(error.message, "error");
    }
  }

  async function clearLog() {
    if (!state.logs.job) return;
    const name = state.logs.job;
    const confirmed = await confirmAction({
      title: "Limpar log",
      message: `Apagar todo o conteúdo do log de "${name}"? Esta ação não pode ser desfeita.`,
      acceptLabel: "Limpar"
    });
    if (!confirmed) return;
    try {
      await api(`/api/jobs/${encodedName(name)}/logs`, { method: "DELETE" });
      state.logs.text = "";
      state.logs.offset = 0;
      state.logs.size = 0;
      state.logs.updatedAt = Date.now();
      renderLog();
      toast("Log limpo.", "success");
    } catch (error) {
      toast(error.message, "error");
    }
  }

  /* ---------------------------------------------------------------------- */
  /* Toasts e confirmação                                                   */
  /* ---------------------------------------------------------------------- */

  const TOAST_ICONS = { success: "#i-check", error: "#i-alert", info: "#i-activity" };

  function toast(message, type = "info") {
    const node = document.createElement("div");
    node.className = `toast ${type}`;
    node.innerHTML = `<svg aria-hidden="true"><use href="${TOAST_ICONS[type] || TOAST_ICONS.info}"/></svg><span></span>`;
    node.querySelector("span").textContent = message;
    el.toasts.append(node);
    setTimeout(() => {
      node.classList.add("leaving");
      setTimeout(() => node.remove(), 240);
    }, 4200);
  }

  function confirmAction({ title, message, acceptLabel = "Confirmar", danger = true }) {
    el.confirmTitle.textContent = title;
    el.confirmMessage.textContent = message;
    el.confirmAccept.textContent = acceptLabel;
    el.confirmAccept.className = `button ${danger ? "danger" : ""}`.trim();
    el.confirmDialog.returnValue = "";
    return new Promise(resolve => {
      const onClose = () => {
        el.confirmDialog.removeEventListener("close", onClose);
        resolve(el.confirmDialog.returnValue === "accept");
      };
      el.confirmDialog.addEventListener("close", onClose);
      el.confirmDialog.showModal();
    });
  }

  /* ---------------------------------------------------------------------- */
  /* Conexão                                                                */
  /* ---------------------------------------------------------------------- */

  async function connect(rawToken) {
    const token = String(rawToken ?? "").trim();
    if (!token) {
      toast("Informe o token configurado no servidor.", "error");
      return false;
    }
    setToken(token);
    try {
      await refreshData({ silent: false });
      setConnection("online", "Sessão autenticada");
      el.tokenInput.value = "";
      toast("Conexão estabelecida.", "success");
      return true;
    } catch (error) {
      // Do not keep an invalid credential in session storage.
      setToken("");
      setConnection("offline", "Falha na autenticação");
      toast(error.message, "error");
      return false;
    }
  }

  function disconnect() {
    setToken("");
    state.jobs = [];
    state.running = new Set();
    state.info = null;
    state.loadedOnce = false;
    state.jobsPage = { limit: 25, offset: 0, total: 0, q: "", status: "todas" };
    state.pickerJobs = [];
    state.history.items = [];
    state.history.total = 0;
    state.history.offset = 0;
    clearLogSelection();
    setConnection("offline", "Informe o token da API");
    el.tokenInput.value = "";
    el.quickToken.value = "";
    el.searchInput.value = "";
    el.statusFilter.value = "todas";
    el.backupsSummary.textContent = "Conecte-se para listar os backups.";
    el.backupsBody.innerHTML = `<tr><td class="table-message" colspan="6">Conecte-se para listar os backups.</td></tr>`;
    el.backupVerificationsSummary.textContent = "Conecte-se para verificar o histórico.";
    el.backupVerificationsBody.innerHTML = `<tr><td class="table-message" colspan="4">Conecte-se para verificar o histórico.</td></tr>`;
    el.backupHealthStatus.className = "result-badge";
    el.backupHealthStatus.textContent = "—";
    el.backupHealthTitle.textContent = "Status dos backups";
    el.backupHealthDetails.textContent = "Conecte-se para carregar o resumo.";
    renderAll();
    toast("Sessão encerrada.", "info");
  }

  /* ---------------------------------------------------------------------- */
  /* Listeners                                                              */
  /* ---------------------------------------------------------------------- */

  function bindEvents() {
    el.menuToggle.addEventListener("click", openNav);
    el.scrim.addEventListener("click", closeNav);
    document.addEventListener("keydown", event => {
      if (event.key === "Escape") {
        closeNav();
        return;
      }
      if (el.jobDialog.open || el.confirmDialog.open) return;
      const tag = (event.target.tagName || "").toLowerCase();
      const typing = ["input", "textarea", "select"].includes(tag) || event.target.isContentEditable;
      if (typing) {
        if (event.key === "Escape") event.target.blur();
        return;
      }
      if (event.key === "n" && state.view !== "config") { event.preventDefault(); openCreateDialog(); }
      if (event.key === "/" && state.view === "tarefas") { event.preventDefault(); el.searchInput.focus(); }
      if (event.key === "r") { event.preventDefault(); globalRefresh(); }
    });

    window.addEventListener("hashchange", () => setView(viewFromHash()));
    document.addEventListener("visibilitychange", () => {
      if (!document.hidden && state.view === "logs" && state.logs.job) {
        pollLog();
        startLogPolling();
      }
    });

    el.globalRefresh.addEventListener("click", globalRefresh);
    el.newJobButton.addEventListener("click", openCreateDialog);
    el.quickConnectForm.addEventListener("submit", event => {
      event.preventDefault();
      connect(el.quickToken.value);
    });
    el.connectionForm.addEventListener("submit", event => {
      event.preventDefault();
      connect(el.tokenInput.value);
    });
    el.disconnectButton.addEventListener("click", disconnect);
    el.infoRefresh.addEventListener("click", async () => {
      if (!getToken()) { toast("Conecte-se para consultar o servidor.", "error"); return; }
      try {
        await refreshData({ silent: true });
        await loadBackups();
        await loadBackupVerifications();
        await loadBackupHealth();
        toast("Informações atualizadas.", "success");
      } catch (error) {
        toast(error.message, "error");
      }
    });
    el.backupsRefresh.addEventListener("click", () => {
      loadBackups().catch(error => toast(error.message, "error"));
      loadBackupVerifications().catch(error => toast(error.message, "error"));
      loadBackupHealth().catch(error => toast(error.message, "error"));
    });
    el.backupsExportJson.addEventListener("click", () => {
      exportBackupHealth("json").catch(error => toast(error.message, "error"));
    });
    el.backupsExportCsv.addEventListener("click", () => {
      exportBackupHealth("csv").catch(error => toast(error.message, "error"));
    });
    el.backupsSendReport.addEventListener("click", async () => {
      if (!getToken()) {
        toast("Conecte-se à API para enviar.", "error");
        return;
      }
      try {
        await api("/api/backups/health/report", { method: "POST" });
        toast("Relatório de saúde enviado.", "success");
        await loadBackupHealth();
      } catch (error) {
        toast(error.message, "error");
      }
    });
    el.backupsVerify.addEventListener("click", async () => {
      if (!getToken()) {
        toast("Conecte-se à API para verificar.", "error");
        return;
      }
      try {
        const result = await api("/api/backups/verify", { method: "POST" });
        toast(
          `Verificação remota: ${result.ok}/${result.checked} OK` +
          (result.failed ? `, ${result.failed} falha(s).` : "."),
          result.failed ? "error" : "success"
        );
        await refreshData({ silent: true });
        await loadBackups();
        await loadBackupVerifications();
        await loadBackupHealth();
      } catch (error) {
        toast(error.message, "error");
      }
    });
    el.backupsBody.addEventListener("click", event => {
      const button = event.target.closest("button[data-backup]");
      if (!button || button.disabled) return;
      restoreBackup(button.dataset.backup).catch(error => toast(error.message, "error"));
    });

    el.exportJobs.addEventListener("click", exportJobs);
    el.importJobs.addEventListener("click", () => el.importFile.click());
    el.importFile.addEventListener("change", () => {
      const file = el.importFile.files?.[0];
      el.importFile.value = "";
      importJobsFile(file).catch(error => toast(error.message, "error"));
    });

    el.searchInput.addEventListener("input", () => {
      state.jobsPage.q = el.searchInput.value.trim();
      state.jobsPage.offset = 0;
      clearTimeout(state.timers.search);
      state.timers.search = setTimeout(() => {
        refreshData({ silent: true }).catch(error => toast(error.message, "error"));
      }, 250);
    });
    el.statusFilter.addEventListener("change", () => {
      state.jobsPage.status = el.statusFilter.value;
      state.jobsPage.offset = 0;
      refreshData({ silent: true }).catch(error => toast(error.message, "error"));
    });
    $$("th[data-sort]").forEach(cell => {
      cell.addEventListener("click", () => {
        const key = cell.dataset.sort;
        if (state.sort.key === key) state.sort.dir *= -1;
        else { state.sort.key = key; state.sort.dir = key === "last" ? -1 : 1; }
        state.jobsPage.offset = 0;
        refreshData({ silent: true }).catch(error => toast(error.message, "error"));
      });
    });
    el.jobsPrev.addEventListener("click", () => {
      state.jobsPage.offset = Math.max(0, state.jobsPage.offset - state.jobsPage.limit);
      refreshData({ silent: true }).catch(error => toast(error.message, "error"));
    });
    el.jobsNext.addEventListener("click", () => {
      state.jobsPage.offset += state.jobsPage.limit;
      refreshData({ silent: true }).catch(error => toast(error.message, "error"));
    });

    el.jobsBody.addEventListener("click", event => {
      const button = event.target.closest("button");
      if (!button) return;
      if (button.dataset.action === "create") {
        openCreateDialog();
        return;
      }
      handleJobAction(button.dataset.action, button.dataset.name);
    });

    el.logJobSearch.addEventListener("input", renderLogJobList);
    el.logJobList.addEventListener("click", event => {
      const button = event.target.closest(".log-job");
      if (button) selectLog(button.dataset.name);
    });

    el.logLive.addEventListener("change", () => {
      state.logs.live = el.logLive.checked;
      el.livePill.hidden = !state.logs.live;
      if (state.logs.live) { pollLog(); startLogPolling(); }
      else stopLogPolling();
    });
    el.logAutoScroll.addEventListener("change", () => {
      state.logs.autoScroll = el.logAutoScroll.checked;
      if (state.logs.autoScroll) el.logOutput.scrollTop = el.logOutput.scrollHeight;
    });
    el.logWrap.addEventListener("change", () => updatePrefs({ wrapLogs: el.logWrap.checked }));
    el.logFilter.addEventListener("input", () => {
      state.logs.filter = el.logFilter.value;
      renderLog();
    });
    el.logRefresh.addEventListener("click", loadLogFull);
    el.logDownload.addEventListener("click", downloadLog);
    el.logClear.addEventListener("click", clearLog);

    el.prefAutoRefresh.addEventListener("change", () => {
      const value = Math.max(0, Math.min(3600, Number(el.prefAutoRefresh.value) || 0));
      el.prefAutoRefresh.value = String(value);
      updatePrefs({ autoRefresh: value });
    });
    el.prefLogPoll.addEventListener("change", () => {
      const value = Math.max(1, Math.min(60, Number(el.prefLogPoll.value) || 2));
      el.prefLogPoll.value = String(value);
      updatePrefs({ logPoll: value });
    });
    el.prefAnimations.addEventListener("change", () => updatePrefs({ animations: el.prefAnimations.checked }));
    el.prefCompact.addEventListener("change", () => updatePrefs({ compact: el.prefCompact.checked }));
    el.prefWrap.addEventListener("change", () => updatePrefs({ wrapLogs: el.prefWrap.checked }));

    el.historyJobFilter.addEventListener("change", () => {
      state.history.job = el.historyJobFilter.value;
      loadHistory({ reset: true }).catch(error => toast(error.message, "error"));
    });
    el.historyResultFilter.addEventListener("change", () => {
      state.history.result = el.historyResultFilter.value;
      loadHistory({ reset: true }).catch(error => toast(error.message, "error"));
    });
    el.historyRefresh.addEventListener("click", () => {
      loadHistory({ reset: true, silent: false }).catch(error => toast(error.message, "error"));
    });
    el.historyPrev.addEventListener("click", () => {
      state.history.offset = Math.max(0, state.history.offset - state.history.limit);
      loadHistory().catch(error => toast(error.message, "error"));
    });
    el.historyNext.addEventListener("click", () => {
      state.history.offset += state.history.limit;
      loadHistory().catch(error => toast(error.message, "error"));
    });

    el.jobForm.addEventListener("submit", saveJob);
    el.closeJobDialog.addEventListener("click", () => el.jobDialog.close());
    el.cancelJobButton.addEventListener("click", () => el.jobDialog.close());
    el.confirmCancel.addEventListener("click", () => el.confirmDialog.close("cancel"));
    el.confirmAccept.addEventListener("click", () => el.confirmDialog.close("accept"));

    el.scheduleType.addEventListener("change", () => setScheduleForm(el.scheduleType.value));
    el.scheduleFields.addEventListener("input", updateSchedulePreview);
    el.scheduleFields.addEventListener("change", updateSchedulePreview);

    $$("dialog").forEach(dialog => {
      dialog.addEventListener("click", event => {
        if (event.target === dialog) dialog.close();
      });
    });
  }

  async function globalRefresh() {
    if (!getToken()) {
      toast("Conecte-se à API para atualizar.", "error");
      setView("config");
      return;
    }
    el.globalRefresh.classList.add("spin");
    try {
      await refreshData({ silent: true });
      if (state.view === "logs" && state.logs.job) await loadLogFull();
      if (state.view === "historico") await loadHistory({ silent: true });
      toast("Dados atualizados.", "success");
    } catch (error) {
      toast(error.message, "error");
    } finally {
      el.globalRefresh.classList.remove("spin");
    }
  }

  /* ---------------------------------------------------------------------- */
  /* Inicialização                                                          */
  /* ---------------------------------------------------------------------- */

  function init() {
    applyPrefs();
    bindEvents();
    setView(viewFromHash());

    if (location.protocol === "file:") {
      el.connectBanner.hidden = false;
      el.quickConnectForm.hidden = true;
      el.connectBanner.querySelector(".notice-body strong").textContent = "Abra o painel pelo servidor";
      el.connectBanner.querySelector(".notice-body span").textContent =
        "Execute `cargo run --release` e acesse http://127.0.0.1:8080. Abrir o arquivo direto no navegador não permite falar com a API.";
      setConnection("offline", "Abra pelo servidor");
      renderTasks();
      return;
    }

    const token = getToken();
    if (token) {
      refreshData({ silent: false }).catch(error => {
        setConnection("offline", "Não foi possível conectar");
        el.jobsBody.innerHTML = `<tr><td class="table-message" colspan="5">${escapeHtml(error.message)}</td></tr>`;
      });
    } else {
      setConnection("offline", "Informe o token da API");
      renderTasks();
    }
  }

  init();
})();
