import { useEffect } from "react";
import Sidebar from "./core/components/Sidebar";
import ContentArea from "./core/components/ContentArea";
import TmuxSetupScreen from "./core/components/TmuxSetupScreen";
import { useTmuxStatus } from "./core/store/tmuxStatusStore";
import "./styles/app.css";

export default function App() {
  const hydrate = useTmuxStatus((s) => s.hydrate);
  const loaded = useTmuxStatus((s) => s.loaded);
  const tmuxAvailable = useTmuxStatus((s) => s.status.available);

  useEffect(() => {
    hydrate();
  }, [hydrate]);

  // While the probe result is still in flight, show nothing — first paint
  // is fast and a transient empty screen beats flashing the setup UI then
  // hiding it.
  if (!loaded) return <div className="app" />;

  if (!tmuxAvailable) {
    return <TmuxSetupScreen />;
  }

  return (
    <div className="app">
      <Sidebar />
      <ContentArea />
    </div>
  );
}
