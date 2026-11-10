import { createToaster } from "panelui-web/toast";

/** App-wide toast manager: `toaster.add({ title, type })` from anywhere, provider or not. */
export const toaster = createToaster();
