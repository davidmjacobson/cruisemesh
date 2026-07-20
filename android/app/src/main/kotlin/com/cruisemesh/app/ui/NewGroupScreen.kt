package com.cruisemesh.app.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material3.Button
import androidx.compose.material3.Checkbox
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import uniffi.cruisemesh_core.Contact
import uniffi.cruisemesh_core.coreContactDisplayName
import uniffi.cruisemesh_core.formatUserId
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.res.pluralStringResource
import com.cruisemesh.app.R

/**
 * Create a group: name + multi-select friends (DESIGN.md §14.6 / §6.5).
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun NewGroupScreen(
    contacts: List<Contact>,
    avatarBytesByUserId: Map<String, ByteArray> = emptyMap(),
    onCreate: (name: String, members: List<Contact>) -> Unit,
    onBack: () -> Unit,
) {
    var name by remember { mutableStateOf("") }
    var selected by remember { mutableStateOf(setOf<String>()) }

    fun contactKey(c: Contact): String = formatUserId(c.userId)

    val canCreate = name.trim().isNotEmpty() && selected.isNotEmpty()

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(stringResource(R.string.ui_new_group)) },
                navigationIcon = {
                    IconButton(onClick = onBack) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
                    }
                },
            )
        },
    ) { innerPadding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(innerPadding)
                .padding(horizontal = 16.dp),
        ) {
            OutlinedTextField(
                value = name,
                onValueChange = { name = it },
                label = { Text(stringResource(R.string.ui_group_name)) },
                singleLine = true,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(top = 8.dp, bottom = 16.dp),
            )

            Text(stringResource(R.string.ui_members),
                style = MaterialTheme.typography.labelMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                modifier = Modifier.padding(bottom = 8.dp),
            )

            if (contacts.isEmpty()) {
                Text(stringResource(R.string.ui_add_friends_before_creating_a_group),
                    style = MaterialTheme.typography.bodyMedium,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            } else {
                LazyColumn(modifier = Modifier.weight(1f)) {
                    items(contacts, key = { contactKey(it) }) { contact ->
                        val key = contactKey(contact)
                        val checked = key in selected
                        val displayId = formatUserId(contact.userId)
                        Row(
                            modifier = Modifier
                                .fillMaxWidth()
                                .clickable {
                                    selected = if (checked) selected - key else selected + key
                                }
                                .padding(vertical = 8.dp),
                            verticalAlignment = Alignment.CenterVertically,
                        ) {
                            Checkbox(
                                checked = checked,
                                onCheckedChange = {
                                    selected = if (it) selected + key else selected - key
                                },
                            )
                            Spacer(modifier = Modifier.width(8.dp))
                            AvatarBadge(
                                userId = contact.userId,
                                name = coreContactDisplayName(contact),
                                displayId = displayId,
                                size = 40.dp,
                                photoBytes = avatarBytesByUserId[displayId],
                            )
                            Spacer(modifier = Modifier.width(12.dp))
                            Text(
                                ChatListLogic.displayNameOrId(coreContactDisplayName(contact), displayId),
                                style = MaterialTheme.typography.bodyLarge.copy(
                                    fontWeight = if (checked) FontWeight.Medium else FontWeight.Normal,
                                ),
                            )
                        }
                    }
                }
            }

            Button(
                onClick = {
                    val members = contacts.filter { contactKey(it) in selected }
                    onCreate(name.trim(), members)
                },
                enabled = canCreate,
                modifier = Modifier
                    .fillMaxWidth()
                    .padding(vertical = 16.dp),
            ) {
                val createLabel = if (selected.isEmpty()) {
                    stringResource(R.string.ui_create_group)
                } else {
                    pluralStringResource(R.plurals.ui_create_group_count, selected.size, selected.size)
                }
                Text(createLabel)
            }
        }
    }
}
