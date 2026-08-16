package ai.choosh.resources

import ai.choosh.engine.ChooshEngine
import ai.choosh.engine.MobileProfile
import ai.choosh.engine.Resource
import ai.choosh.engine.ResourceConfirmResult
import ai.choosh.engine.ResourcePattern
import ai.choosh.engine.ResourceProposeResult
import ai.choosh.viewmodel.loadInto
import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch

/** One [ChooshEngine.resourcePropose] this ViewModel itself issued, awaiting the human's own [ChooshEngine.resourceConfirm] — the smoke-test form's round trip, distinct from [ResourceEventTracker.pendingProposals]'s agent-initiated ones. */
data class PendingSelfProposal(val resourceId: String, val displayName: String)

data class ResourcesUiState(
    val resources: List<Resource> = emptyList(),
    val isLoading: Boolean = true,
    val error: String? = null,
    val pendingSelfProposals: List<PendingSelfProposal> = emptyList(),
    val proposeInFlight: Boolean = false,
    val proposeError: String? = null,
)

/**
 * Backs the real per-devhost Resources screen, per
 * docs/specs/resources-and-reauth.md — Resources are devhost-scoped, same
 * "one devhost's own registry" scoping [ai.choosh.fleet.DevHostWorkspacesViewModel]
 * already uses for `workspace.list`.
 *
 * [propose]/[confirmSelfProposal] exercise the full `resource.propose` ->
 * `resource.confirm` round trip as a two-step, human-visible flow (never
 * auto-confirming) — per that RPC family's own confirmation-gate design,
 * `resource.propose` always yields a *pending* id regardless of who
 * initiated it; this screen's own "declare a Resource directly" form is
 * just one caller of that same gate, not a special-cased shortcut around
 * it. [ResourceEventTracker.pendingProposals] is the separate,
 * agent-initiated path into the same [ChooshEngine.resourceConfirm] call.
 */
class ResourcesViewModel(private val engine: ChooshEngine, private val deviceId: String) : ViewModel() {
    private val _state = MutableStateFlow(ResourcesUiState())
    val state: StateFlow<ResourcesUiState> = _state.asStateFlow()

    init {
        refresh()
    }

    fun refresh() {
        viewModelScope.launch {
            _state.loadInto(
                setLoading = { it.copy(isLoading = true, error = null) },
                call = { engine.resourceList(deviceId) },
                onSuccess = { state, resources -> state.copy(resources = resources, isLoading = false, error = null) },
                onFailure = { state, failure -> state.copy(isLoading = false, error = failure.message ?: "failed to load resources") },
            )
        }
    }

    fun propose(displayName: String, resourceKind: String, pattern: ResourcePattern?, reauthCommand: String?, mobileProfile: MobileProfile) {
        if (displayName.isBlank() || resourceKind.isBlank()) {
            _state.value = _state.value.copy(proposeError = "display name and kind are required")
            return
        }
        viewModelScope.launch {
            _state.value = _state.value.copy(proposeInFlight = true, proposeError = null)
            when (val result = engine.resourcePropose(deviceId, displayName, resourceKind, pattern, reauthCommand, mobileProfile)) {
                is ResourceProposeResult.Success -> _state.value = _state.value.copy(
                    proposeInFlight = false,
                    pendingSelfProposals = _state.value.pendingSelfProposals + PendingSelfProposal(result.resourceId, displayName),
                )
                is ResourceProposeResult.Failure -> _state.value = _state.value.copy(proposeInFlight = false, proposeError = result.message)
            }
        }
    }

    fun confirmSelfProposal(resourceId: String, approve: Boolean) {
        viewModelScope.launch {
            when (val result = engine.resourceConfirm(deviceId, resourceId, approve)) {
                is ResourceConfirmResult.Success -> {
                    _state.value = _state.value.copy(pendingSelfProposals = _state.value.pendingSelfProposals.filterNot { it.resourceId == resourceId })
                    refresh()
                }
                is ResourceConfirmResult.Failure -> _state.value = _state.value.copy(proposeError = result.message)
            }
        }
    }
}
