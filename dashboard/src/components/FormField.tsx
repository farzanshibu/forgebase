/**
 * FormField: a label, optional help text and an inline error, wired to the control with `id`, `aria-describedby`
 * and `aria-invalid` (dashboard-ux.md rule 4: form errors appear next to their field; audit A-04).
 *
 * Usage:
 *   const byField = fieldErrors(saveError);                 // from ErrorPanel / errors.ts
 *   <FormField label="Name" help="Letters, digits and underscores." error={validate.collectionName(name) ?? byField.name}>
 *     {(p) => <Input {...p} value={name} onChange={(e) => setName(e.target.value)} />}
 *   </FormField>
 *   <FormField label="Visibility" term="acl">                 // a HelpTip after the label
 *     {(p) => <Sel {...p} value={v} onChange={setV} options={VISIBILITIES} />}
 *   </FormField>
 *
 * The child is a function that receives the control props, so any input (native, PanelUI, Sel) can take them.
 * Show an error only after the user has typed or tried to submit, not on an untouched empty form.
 */
import { useId, type ReactNode } from "react";
import type { GlossaryTerm } from "../glossary";
import { HelpTip } from "./HelpTip";

export interface ControlProps {
  id: string;
  "aria-describedby"?: string;
  "aria-invalid"?: true;
  "aria-required"?: true;
}

export interface FormFieldProps {
  label: ReactNode;
  /** One line of help under the control. Stays visible while an error shows. */
  help?: ReactNode;
  /** The error for this field; empty hides it. */
  error?: string;
  /** Marks the label and the control as required. */
  required?: boolean;
  /** A glossary term to explain with a HelpTip after the label. */
  term?: GlossaryTerm;
  /** Use a fixed id for the control (for example to point a second label at it). */
  id?: string;
  className?: string;
  children: (props: ControlProps) => ReactNode;
}

export function FormField({ label, help, error, required, term, id: fixedId, className = "", children }: FormFieldProps) {
  const auto = useId();
  const id = fixedId ?? `f${auto.replace(/:/g, "")}`;
  const helpId = help ? `${id}-help` : undefined;
  const errorId = error ? `${id}-error` : undefined;
  const describedBy = [errorId, helpId].filter(Boolean).join(" ") || undefined;
  return (
    <div className={`flex flex-col gap-1 text-sm ${className}`.trim()}>
      <span className="flex items-center gap-1">
        <label htmlFor={id} className="font-medium">
          {label}
          {required && (
            <span className="text-destructive" aria-hidden="true">
              {" "}
              *
            </span>
          )}
        </label>
        {term && <HelpTip term={term} />}
      </span>
      {children({ id, "aria-describedby": describedBy, ...(error ? { "aria-invalid": true as const } : {}), ...(required ? { "aria-required": true as const } : {}) })}
      {error && (
        <p id={errorId} className="text-xs font-medium text-destructive">
          {error}
        </p>
      )}
      {help && (
        <p id={helpId} className="text-xs text-muted-foreground">
          {help}
        </p>
      )}
    </div>
  );
}

export default FormField;
