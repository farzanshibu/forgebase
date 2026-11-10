import { listFunctionVersions } from "../../api";
import type { FunctionInfo } from "../../api";
import { ErrorView, Loading } from "../../components/ui";
import { useLoad } from "../../hooks";
import TestPanel from "./TestPanel";

export default function TestTab({ info }: { info: FunctionInfo }) {
  const versions = useLoad((s) => listFunctionVersions(info.name, s), `test-versions:${info.name}:${info.latest_version}`);
  if (versions.error) return <ErrorView error={versions.error} />;
  if (!versions.data) return <Loading what="Loading versions" />;
  return <TestPanel info={info} versions={versions.data} />;
}
