import { Alert } from "panelui-web/alert";
import { Loading, ErrorView } from "../../components/ui";
import FeaturesFlow from "./FeaturesFlow";
import { useSharedFeatures } from "./features";

/** System studio, "Features & settings": the feature map, configure and done steps of first-run setup. */
export default function FeaturesPage() {
  const data = useSharedFeatures();
  if (!data || data.loading) return <Loading what="Features" />;
  if (!data.snapshot) return data.error ? <ErrorView error={data.error} /> : (
    <Alert tone="warning" variant="plain">
      <Alert.Icon />
      <Alert.Title>The feature list is not available</Alert.Title>
    </Alert>
  );
  return (
    <div className="max-w-5xl space-y-2">
      <h1 className="text-2xl font-semibold tracking-tight">Features &amp; settings</h1>
      <p className="mb-4 text-muted-foreground">Switch modules on or off and edit their settings in {data.snapshot.config_file}.</p>
      <FeaturesFlow snapshot={data.snapshot} apply={data.apply} />
    </div>
  );
}
