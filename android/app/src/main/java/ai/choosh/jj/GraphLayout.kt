package ai.choosh.jj

import ai.choosh.engine.ChangeGraphNode

/** One node's position in a purely client-side, presentation-only DAG layout. */
data class GraphPosition(val changeId: String, val row: Int, val column: Int)

/**
 * Computes node positions for [JjChangeGraphScreen] to render — layout
 * only, never diff/log *data*: every [ChangeGraphNode] here came verbatim
 * from `workspace.log`'s response, per M3's "no log computation on
 * Android" exit criterion (this function only decides *where on screen* an
 * already-fetched node goes, the same way a `RecyclerView` layout manager
 * doesn't "compute" the data it arranges).
 *
 * `row` is a generation number (0 = newest, increasing toward older
 * ancestors) derived from each node's longest parent-chain length within
 * the returned node set — a node whose parents aren't present in this same
 * `nodes` list (the revset's boundary) is treated as its own root. `column`
 * breaks ties within a row by `changeId` for a stable, deterministic
 * layout — not a graph-theoretic requirement, just reproducible output.
 */
fun layoutChangeGraph(nodes: List<ChangeGraphNode>): List<GraphPosition> {
    if (nodes.isEmpty()) return emptyList()
    val byId = nodes.associateBy { it.changeId }
    val generationFromRoot = HashMap<String, Int>()

    fun generation(changeId: String, visiting: Set<String>): Int {
        generationFromRoot[changeId]?.let { return it }
        // Defensive only: `workspace.log` should never hand back a real
        // cycle, but a same-generation fallback here means a malformed
        // response degrades to an odd layout, never an infinite loop.
        if (changeId in visiting) return 0
        val node = byId[changeId] ?: return 0
        val parentGenerations = node.parentChangeIds.filter { it in byId }.map { generation(it, visiting + changeId) }
        val result = if (parentGenerations.isEmpty()) 0 else parentGenerations.max() + 1
        generationFromRoot[changeId] = result
        return result
    }

    val generations = nodes.associate { it.changeId to generation(it.changeId, emptySet()) }
    val maxGeneration = generations.values.max()

    return nodes
        .groupBy { maxGeneration - generations.getValue(it.changeId) }
        .flatMap { (row, rowNodes) -> rowNodes.sortedBy { it.changeId }.mapIndexed { column, node -> GraphPosition(node.changeId, row, column) } }
        .sortedWith(compareBy({ it.row }, { it.column }))
}
