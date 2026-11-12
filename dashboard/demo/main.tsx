/**
 * Demo mode (`npm run demo`): the real dashboard with every API call answered in the browser by the same fakes the
 * tests use (src/test/*Mock.ts, checked against openapi.json). No server needed: use it to work on the UI, review a
 * change, or click through the whole journey. Data lives in memory and resets on reload.
 *
 *   http://127.0.0.1:5173/admin/                signed in as root@example.com, every studio with sample data
 *   http://127.0.0.1:5173/admin/?first-run      a fresh server: first-run setup (the token is filled in for you)
 *
 * Sign-in password for the fake administrator: correct-horse.
 */
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { BrowserRouter } from "react-router-dom";
import "@fontsource-variable/inter/wght.css";
import "@fontsource-variable/jetbrains-mono/wght.css";
import "../src/index.css";
import App from "../src/App";
import { AuthFake } from "../src/pages/auth/authMock";
import { registerAccess } from "../src/pages/access/accessMock";
import { signedIn } from "../src/test/helpers";
import { installBackups } from "../src/test/backupMock";
import { installBilling } from "../src/test/billingMock";
import { installFunctions, installJobs } from "../src/test/fnMock";
import { installIdentity } from "../src/test/idMock";
import { installKv } from "../src/test/kvMock";
import { installOverview } from "../src/test/overviewMock";
import { installPg } from "../src/test/pgMock";
import { installQueues } from "../src/test/queueMock";
import { installSetup, SETUP_TOKEN } from "../src/test/setupMock";
import { installStorage } from "../src/test/storageMock";
import { installTopology } from "../src/test/topologyMock";
import { installGraph, installVector } from "../src/test/vgMock";

const firstRun = new URLSearchParams(window.location.search).has("first-run");
const fake = new AuthFake();
const mock = signedIn(fake.mock, !firstRun);
const setup = installSetup(mock, { required: firstRun });
for (const f of setup.features) {
  f.enabled = true;
  for (const s of f.settings) if (s.key === `${f.id}.enabled`) s.value = true;
}
const installers: [string, () => unknown][] = [
  ["overview", () => installOverview(mock)],
  ["pg", () => installPg(mock)],
  ["storage", () => installStorage(mock)],
  ["functions", () => installFunctions(mock)],
  ["jobs", () => installJobs(mock)],
  ["queues", () => installQueues(mock)],
  ["kv", () => installKv(mock)],
  ["vector", () => installVector(mock)],
  ["graph", () => installGraph(mock)],
  ["identity", () => installIdentity(mock)],
  ["access", () => registerAccess(mock)],
  ["billing", () => installBilling(mock)],
  ["backups", () => installBackups(mock)],
  ["topology", () => installTopology(mock)],
];
for (const [name, install] of installers) {
  try {
    install();
  } catch (e) {
    console.warn(`demo: ${name} mock failed to install`, e);
  }
}
(window as unknown as { __demo: unknown }).__demo = { mock, fake, setup };
if (firstRun) {
  // What `forgebase serve` prints on a fresh server: the setup link with its token.
  window.history.replaceState(null, "", `/admin/setup?token=${SETUP_TOKEN}`);
  console.info(`ForgeBase demo: first-run setup. Setup token ${SETUP_TOKEN}.`);
}
console.info("ForgeBase demo mode: API calls are answered by in-browser fakes (dashboard/demo). Reload to reset.", (window as unknown as { __demo: unknown }).__demo);

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <BrowserRouter basename="/admin">
      <App />
    </BrowserRouter>
  </StrictMode>,
);
