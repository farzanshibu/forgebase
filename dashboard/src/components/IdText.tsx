/**
 * IdText: a long id in monospace, cut to fit with the whole id on hover, and a copy icon. The same in every studio.
 *
 * Usage:
 *   <IdText id={key.id} what="key id" />          // copy button named "Copy key id"
 *   <IdText id={job.id} noun="job" />             // "Copy job id"
 *   <IdText value={payment.id} what="payment id" />
 *   <IdText id={u.id} copy={false} />             // just the cut-to-fit id
 *
 * `value` is the same as `id`. Without `what` or `noun` the button reads "Copy id". `children` replaces the shown text
 * (the copy still copies the id).
 */
import type { ReactNode } from "react";
import { CopyIcon } from "./Button";

export function IdText({ id, value, what, noun, copy = true, className = "", children }: { id?: string; value?: string; what?: string; noun?: string; copy?: boolean; className?: string; children?: ReactNode }) {
  const text = id ?? value ?? "";
  const label = what ? `Copy ${what}` : noun ? `Copy ${noun} id` : "Copy id";
  return (
    <span className={`inline-flex min-w-0 max-w-full items-center gap-0.5 align-middle ${className}`.trim()}>
      <span title={text} className="min-w-0 max-w-[16rem] truncate font-mono text-xs">
        {children ?? text}
      </span>
      {copy && text && <CopyIcon text={text} label={label} />}
    </span>
  );
}
