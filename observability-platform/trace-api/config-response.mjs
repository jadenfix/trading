export function buildApiConfigResponse(options) {
  return {
    broker_base_url: options.brokerBaseUrl,
    temporal_ui_url: options.temporalUiUrl,
    trace_api_url: options.traceApiUrl,
    api_version: "v1",
    default_project: options.defaultProject,
    default_location: options.defaultLocation,
    control_token_required: true,
    control_token_default: null,
  };
}
