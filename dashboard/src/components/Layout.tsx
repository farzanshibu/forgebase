import { useState } from "react";
import { NavLink, Outlet } from "react-router-dom";
import ImpersonationBanner from "../auth/ImpersonationBanner";
import { useSession } from "../auth/session";
import { STUDIOS } from "../studios";

export default function Layout() {
  const [open, setOpen] = useState(false);
  const { admin, signOut } = useSession();
  return (
    <div className="flex min-h-screen flex-col">
      <ImpersonationBanner />
      <div className="flex min-h-0 flex-1">
      <aside
        className={`fixed inset-y-0 left-0 z-20 flex w-60 shrink-0 flex-col border-r border-zinc-800 bg-zinc-900 transition-transform md:static md:translate-x-0 ${open ? "translate-x-0" : "-translate-x-full"}`}
      >
        <div className="flex h-14 items-center border-b border-zinc-800 px-4 font-semibold tracking-tight">
          Forge<span className="text-amber-400">Base</span>
          <span className="ml-2 text-xs font-normal text-zinc-500">admin</span>
        </div>
        <nav aria-label="Studios" className="flex flex-col gap-0.5 p-2">
          {STUDIOS.map((s) => (
            <NavLink
              key={s.path}
              to={s.path === "" ? "/" : `/${s.path}`}
              end={s.path === ""}
              onClick={() => setOpen(false)}
              className={({ isActive }) =>
                `flex items-center justify-between rounded-md px-3 py-2 text-sm ${isActive ? "bg-zinc-800 text-white" : "text-zinc-400 hover:bg-zinc-800/60 hover:text-zinc-100"}`
              }
            >
              <span>{s.label}</span>
              {s.phase !== null && <span className="text-[10px] uppercase tracking-wide text-zinc-600">P{s.phase}</span>}
            </NavLink>
          ))}
        </nav>
        <div className="mt-auto border-t border-zinc-800 p-3 text-xs text-zinc-400">
          <p className="truncate" title={admin?.email}>{admin?.email}</p>
          <button type="button" onClick={() => void signOut()} className="mt-2 rounded-md border border-zinc-700 px-2 py-1 text-zinc-200 hover:bg-zinc-800">
            Sign out
          </button>
        </div>
      </aside>
      <div className="flex min-w-0 flex-1 flex-col">
        <header className="flex h-14 items-center border-b border-zinc-800 px-4 md:hidden">
          <button
            type="button"
            aria-label="Toggle navigation"
            onClick={() => setOpen((o) => !o)}
            className="rounded-md border border-zinc-700 px-2 py-1 text-sm"
          >
            Menu
          </button>
        </header>
        <main className="min-w-0 flex-1 p-6 md:p-8">
          <Outlet />
        </main>
      </div>
      </div>
    </div>
  );
}
